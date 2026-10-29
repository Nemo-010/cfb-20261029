//! The wire protocol between `cfbd` and `cfb-manager`.
//!
//! A frame is a `u32` little-endian header length, a UTF-8 header, and an optional payload:
//!
//! ```text
//! frame  := header_len:u32le  header[header_len]  payload[?]
//! header := tag "|" field ( ";" field )*
//! field  := key "=" value
//! ```
//!
//! Field values MUST NOT contain `|`, `;` or `=`, and list elements additionally MUST NOT
//! contain `,` or `:`. There is therefore no escaping and no quoting anywhere in the
//! protocol; an encoder that is handed a value containing a separator returns
//! [`ProtocolError::InvalidField`] instead of emitting a frame that would decode as something
//! else. Binary data travels only in `DATA` frames, whose header carries `len` and whose
//! payload follows the header unescaped.

use std::fmt;

pub const PROTOCOL_VERSION: u16 = 1;

pub type Epoch = u64;
pub type MsgId = u64;
pub type StreamId = u32;

/// A 128-bit client event id, used to deduplicate replayed commands.
pub type EventId = [u8; 16];

/// Upper bound on a header, so a corrupt length cannot make the decoder allocate wildly.
const MAX_HEADER: usize = 64 * 1024;

/// Upper bound on a `DATA` payload. A stream negotiates larger transfers with credit.
const MAX_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    UnknownCapability,
    Denied,
    NotFound,
    Busy,
    Disconnected,
    Fenced,
    Timeout,
    StorageUnavailable,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnknownCapability => "unknown_capability",
            ErrorCode::Denied => "denied",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Busy => "busy",
            ErrorCode::Disconnected => "disconnected",
            ErrorCode::Fenced => "fenced",
            ErrorCode::Timeout => "timeout",
            ErrorCode::StorageUnavailable => "storage_unavailable",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorCode> {
        Some(match s {
            "unknown_capability" => ErrorCode::UnknownCapability,
            "denied" => ErrorCode::Denied,
            "not_found" => ErrorCode::NotFound,
            "busy" => ErrorCode::Busy,
            "disconnected" => ErrorCode::Disconnected,
            "fenced" => ErrorCode::Fenced,
            "timeout" => ErrorCode::Timeout,
            "storage_unavailable" => ErrorCode::StorageUnavailable,
            _ => return None,
        })
    }

    /// Whether the caller may retry the same call.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::Busy
                | ErrorCode::Disconnected
                | ErrorCode::Timeout
                | ErrorCode::StorageUnavailable
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadState {
    Pending,
    Starting,
    Running,
    Succeeded,
    Failed,
    Stopped,
}

impl WorkloadState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkloadState::Pending => "pending",
            WorkloadState::Starting => "starting",
            WorkloadState::Running => "running",
            WorkloadState::Succeeded => "succeeded",
            WorkloadState::Failed => "failed",
            WorkloadState::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> Option<WorkloadState> {
        Some(match s {
            "pending" => WorkloadState::Pending,
            "starting" => WorkloadState::Starting,
            "running" => WorkloadState::Running,
            "succeeded" => WorkloadState::Succeeded,
            "failed" => WorkloadState::Failed,
            "stopped" => WorkloadState::Stopped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    pub id: String,
    pub generation: u64,
    pub state: WorkloadState,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    /// Unix seconds at which the current instance started.
    pub started_at: Option<u64>,
    /// CPU time consumed, in millicores-seconds.
    pub cpu_millis: Option<u64>,
    /// Resident memory, in MiB.
    pub memory_mib: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capacity {
    pub cpu_millis: u64,
    pub memory_mib: u64,
    pub disk_free_mib: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// agent → control plane: first frame after connect.
    Hello {
        protocol: u16,
        agent_version: String,
        node_id: String,
        caps: Vec<String>,
    },
    /// control plane → agent: session accepted, with the lost threshold it will apply.
    HelloAck {
        protocol: u16,
        lost_ms: u64,
        retry_after_ms: Option<u64>,
    },
    /// Both directions: the primary liveness signal.
    Watchdog {
        epoch: Epoch,
    },
    /// Capability call.
    Req {
        msg_id: MsgId,
        capability: String,
        args: Vec<(String, String)>,
        /// Client event id, so a replayed command is deduplicated before dispatch.
        event_id: Option<EventId>,
    },
    /// Reply to a `REQ`, possibly out of order.
    Res {
        msg_id: MsgId,
        ok: bool,
        result: String,
        error: Option<CallError>,
    },
    Open {
        stream_id: StreamId,
        capability: String,
        args: Vec<(String, String)>,
        credit: u32,
    },
    /// Stream bytes. The payload is length-delimited and unescaped.
    Data {
        stream_id: StreamId,
        seq: u64,
        payload: Vec<u8>,
    },
    Window {
        stream_id: StreamId,
        credit: u32,
    },
    Close {
        stream_id: StreamId,
        reason: String,
    },
    /// Full node and workload report, for reconciliation.
    State {
        conn_id: u64,
        generation: u64,
        workloads: Vec<Workload>,
        capacity: Capacity,
        caps: Vec<String>,
    },
    /// Host metrics, pushed every `metrics_interval_ms`.
    Metrics {
        epoch: Epoch,
        cpu_pct: f32,
        mem_mb: u64,
        load1: f32,
    },
    /// Cell → node table, tagged with a revision.
    Routes {
        rev: u64,
        entries: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// More bytes are needed; the buffer does not yet hold a whole frame.
    Incomplete,
    /// The frame is structurally invalid.
    BadFrame(String),
    /// A field value contains a separator.
    InvalidField(String),
    /// A field holds a value of the wrong shape.
    BadValue(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Incomplete => f.write_str("incomplete frame"),
            ProtocolError::BadFrame(m) => write!(f, "bad frame: {m}"),
            ProtocolError::InvalidField(m) => write!(f, "invalid field: {m}"),
            ProtocolError::BadValue(m) => write!(f, "bad value: {m}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// A value that may appear as a single field.
fn scalar(v: &str) -> Result<&str, ProtocolError> {
    if v.contains(['|', ';', '=']) {
        return Err(ProtocolError::InvalidField(format!(
            "`{v}` contains a field separator"
        )));
    }
    Ok(v)
}

/// A value that may appear as an element of a comma-separated list.
fn element(v: &str) -> Result<&str, ProtocolError> {
    scalar(v)?;
    if v.contains([',', ':']) {
        return Err(ProtocolError::InvalidField(format!(
            "`{v}` contains a list separator"
        )));
    }
    Ok(v)
}

fn join<T: AsRef<str>>(items: &[T]) -> Result<String, ProtocolError> {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(element(item.as_ref())?);
    }
    Ok(out)
}

fn frame(tag: &str, fields: Vec<(&str, String)>, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let mut header = String::with_capacity(48);
    header.push_str(tag);
    if !fields.is_empty() {
        header.push('|');
        for (i, (k, v)) in fields.iter().enumerate() {
            if i > 0 {
                header.push(';');
            }
            header.push_str(k);
            header.push('=');
            header.push_str(scalar(v)?);
        }
    }
    if header.len() > MAX_HEADER {
        return Err(ProtocolError::BadFrame("header too long".into()));
    }

    let mut out = Vec::with_capacity(4 + header.len() + payload.len());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

impl Message {
    /// Serialise to one frame.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Message::Hello {
                protocol,
                agent_version,
                node_id,
                caps,
            } => frame(
                "HELLO",
                vec![
                    ("protocol", protocol.to_string()),
                    ("agent_version", agent_version.clone()),
                    ("node_id", node_id.clone()),
                    ("caps", join(caps)?),
                ],
                &[],
            ),
            Message::HelloAck {
                protocol,
                lost_ms,
                retry_after_ms,
            } => {
                let mut f = vec![
                    ("protocol", protocol.to_string()),
                    ("lost_ms", lost_ms.to_string()),
                ];
                if let Some(r) = retry_after_ms {
                    f.push(("retry_after_ms", r.to_string()));
                }
                frame("HELLO_ACK", f, &[])
            }
            Message::Watchdog { epoch } => {
                frame("WATCHDOG", vec![("epoch", epoch.to_string())], &[])
            }
            Message::Req {
                msg_id,
                capability,
                args,
                event_id,
            } => {
                let mut encoded = String::new();
                for (i, (k, v)) in args.iter().enumerate() {
                    if i > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(element(k)?);
                    encoded.push(':');
                    encoded.push_str(element(v)?);
                }
                let mut f = vec![
                    ("msg_id", msg_id.to_string()),
                    ("capability", capability.clone()),
                    ("args", encoded),
                ];
                if let Some(id) = event_id {
                    f.push(("event_id", hex_encode(id)));
                }
                frame("REQ", f, &[])
            }
            Message::Res {
                msg_id,
                ok,
                result,
                error,
            } => {
                let mut f = vec![
                    ("msg_id", msg_id.to_string()),
                    ("ok", ok.to_string()),
                    ("result", result.clone()),
                ];
                if let Some(e) = error {
                    f.push((
                        "error",
                        format!("{}:{}:{}", e.code, e.message, e.code.retryable()),
                    ));
                }
                frame("RES", f, &[])
            }
            Message::Open {
                stream_id,
                capability,
                args,
                credit,
            } => {
                let mut encoded = String::new();
                for (i, (k, v)) in args.iter().enumerate() {
                    if i > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(element(k)?);
                    encoded.push(':');
                    encoded.push_str(element(v)?);
                }
                frame(
                    "OPEN",
                    vec![
                        ("stream_id", stream_id.to_string()),
                        ("capability", capability.clone()),
                        ("args", encoded),
                        ("credit", credit.to_string()),
                    ],
                    &[],
                )
            }
            Message::Data {
                stream_id,
                seq,
                payload,
            } => frame(
                "DATA",
                vec![
                    ("stream_id", stream_id.to_string()),
                    ("seq", seq.to_string()),
                    ("len", payload.len().to_string()),
                ],
                payload,
            ),
            Message::Window { stream_id, credit } => frame(
                "WINDOW",
                vec![
                    ("stream_id", stream_id.to_string()),
                    ("credit", credit.to_string()),
                ],
                &[],
            ),
            Message::Close { stream_id, reason } => frame(
                "CLOSE",
                vec![
                    ("stream_id", stream_id.to_string()),
                    ("reason", reason.clone()),
                ],
                &[],
            ),
            Message::State {
                conn_id,
                generation,
                workloads,
                capacity,
                caps,
            } => {
                let mut encoded = String::new();
                for (i, w) in workloads.iter().enumerate() {
                    if i > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(element(&w.id)?);
                    encoded.push(':');
                    encoded.push_str(w.state.as_str());
                    encoded.push(':');
                    encoded.push_str(&w.generation.to_string());
                    encoded.push(':');
                    encoded.push_str(&w.pid.map(|p| p.to_string()).unwrap_or_default());
                    encoded.push(':');
                    encoded.push_str(&w.exit_code.map(|c| c.to_string()).unwrap_or_default());
                    encoded.push(':');
                    encoded.push_str(&w.started_at.map(|v| v.to_string()).unwrap_or_default());
                    encoded.push(':');
                    encoded.push_str(&w.cpu_millis.map(|v| v.to_string()).unwrap_or_default());
                    encoded.push(':');
                    encoded.push_str(&w.memory_mib.map(|v| v.to_string()).unwrap_or_default());
                }
                frame(
                    "STATE",
                    vec![
                        ("conn_id", conn_id.to_string()),
                        ("generation", generation.to_string()),
                        ("workloads", encoded),
                        ("cpu_millis", capacity.cpu_millis.to_string()),
                        ("memory_mib", capacity.memory_mib.to_string()),
                        ("disk_free_mib", capacity.disk_free_mib.to_string()),
                        ("caps", join(caps)?),
                    ],
                    &[],
                )
            }
            Message::Metrics {
                epoch,
                cpu_pct,
                mem_mb,
                load1,
            } => frame(
                "METRICS",
                vec![
                    ("epoch", epoch.to_string()),
                    // `{}` is the shortest representation that round-trips an f32 exactly;
                    // a fixed precision would lose the value.
                    ("cpu", format!("{cpu_pct}")),
                    ("mem_mb", mem_mb.to_string()),
                    ("load1", format!("{load1}")),
                ],
                &[],
            ),
            Message::Routes { rev, entries } => {
                let mut encoded = String::new();
                for (i, (cell, node)) in entries.iter().enumerate() {
                    if i > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(element(cell)?);
                    encoded.push(':');
                    encoded.push_str(element(node)?);
                }
                frame(
                    "ROUTES",
                    vec![("rev", rev.to_string()), ("entries", encoded)],
                    &[],
                )
            }
        }
    }

    /// Parse one frame from the front of `buf`.
    ///
    /// Returns the message and the number of bytes consumed, so a caller can drive this from
    /// a growing read buffer.
    pub fn decode(buf: &[u8]) -> Result<(Message, usize), ProtocolError> {
        if buf.len() < 4 {
            return Err(ProtocolError::Incomplete);
        }
        let header_len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if header_len > MAX_HEADER {
            return Err(ProtocolError::BadFrame(format!(
                "header length {header_len} exceeds {MAX_HEADER}"
            )));
        }
        if buf.len() < 4 + header_len {
            return Err(ProtocolError::Incomplete);
        }
        let header = std::str::from_utf8(&buf[4..4 + header_len])
            .map_err(|_| ProtocolError::BadFrame("header is not UTF-8".into()))?;

        let (tag, rest) = match header.split_once('|') {
            Some((t, r)) => (t, r),
            None => (header, ""),
        };
        let fields = parse_fields(rest)?;

        let payload_len = if tag == "DATA" {
            let raw = get(&fields, "len")?;
            let len: usize = raw
                .parse()
                .map_err(|_| ProtocolError::BadValue(format!("len `{raw}`")))?;
            if len > MAX_PAYLOAD {
                return Err(ProtocolError::BadFrame(format!(
                    "payload length {len} exceeds {MAX_PAYLOAD}"
                )));
            }
            len
        } else {
            if get_opt(&fields, "len").is_some() {
                return Err(ProtocolError::BadFrame(format!(
                    "`len` is only valid on DATA, not {tag}"
                )));
            }
            0
        };

        // Both the header and the payload come off the wire, so the total is computed with
        // checked arithmetic: a hostile length must produce an error, not a panic.
        let end = 4usize
            .checked_add(header_len)
            .and_then(|n| n.checked_add(payload_len))
            .ok_or_else(|| ProtocolError::BadFrame("frame length overflow".into()))?;
        if buf.len() < end {
            return Err(ProtocolError::Incomplete);
        }
        let payload = &buf[4 + header_len..end];

        let message = build(tag, &fields, payload)?;
        Ok((message, end))
    }
}

fn parse_fields(rest: &str) -> Result<Vec<(String, String)>, ProtocolError> {
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for part in rest.split(';') {
        if part.is_empty() {
            return Err(ProtocolError::BadFrame("empty field".into()));
        }
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| ProtocolError::BadFrame(format!("field `{part}` has no `=`")))?;
        if k.is_empty() {
            return Err(ProtocolError::BadFrame("empty field key".into()));
        }
        if out.iter().any(|(seen, _)| seen == k) {
            return Err(ProtocolError::BadFrame(format!("duplicate field `{k}`")));
        }
        // A value the encoder would refuse must not be accepted here either, or the two
        // directions disagree about what a frame means.
        scalar(v)?;
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

fn get<'a>(fields: &'a [(String, String)], key: &str) -> Result<&'a str, ProtocolError> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| ProtocolError::BadFrame(format!("missing field `{key}`")))
}

fn get_opt<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn num<T: std::str::FromStr>(fields: &[(String, String)], key: &str) -> Result<T, ProtocolError> {
    let raw = get(fields, key)?;
    raw.parse::<T>()
        .map_err(|_| ProtocolError::BadValue(format!("{key}=`{raw}`")))
}

fn list(fields: &[(String, String)], key: &str) -> Result<Vec<String>, ProtocolError> {
    let raw = get_opt(fields, key).unwrap_or("");
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    Ok(raw.split(',').map(String::from).collect())
}

fn build(tag: &str, fields: &[(String, String)], payload: &[u8]) -> Result<Message, ProtocolError> {
    match tag {
        "HELLO" => Ok(Message::Hello {
            protocol: num(fields, "protocol")?,
            agent_version: get(fields, "agent_version")?.to_string(),
            node_id: get(fields, "node_id")?.to_string(),
            caps: list(fields, "caps")?,
        }),
        "HELLO_ACK" => Ok(Message::HelloAck {
            protocol: num(fields, "protocol")?,
            lost_ms: num(fields, "lost_ms")?,
            retry_after_ms: match get_opt(fields, "retry_after_ms") {
                Some(raw) => Some(
                    raw.parse()
                        .map_err(|_| ProtocolError::BadValue(format!("retry_after_ms=`{raw}`")))?,
                ),
                None => None,
            },
        }),
        "WATCHDOG" => Ok(Message::Watchdog {
            epoch: num(fields, "epoch")?,
        }),
        "REQ" => Ok(Message::Req {
            msg_id: num(fields, "msg_id")?,
            capability: get(fields, "capability")?.to_string(),
            args: pairs(get_opt(fields, "args").unwrap_or(""))?,
            event_id: match get_opt(fields, "event_id") {
                Some(raw) => Some(hex_decode(raw)?),
                None => None,
            },
        }),
        "RES" => {
            let error = match get_opt(fields, "error") {
                Some(raw) => {
                    // `code:message:retryable`; the message is peeled from the right so it may
                    // itself contain a colon.
                    let (rest, retryable) = raw
                        .rsplit_once(':')
                        .ok_or_else(|| ProtocolError::BadValue(format!("error `{raw}`")))?;
                    match retryable {
                        "true" | "false" => {}
                        other => {
                            return Err(ProtocolError::BadValue(format!(
                                "error retryable `{other}`"
                            )))
                        }
                    }
                    let (code, message) = rest
                        .split_once(':')
                        .ok_or_else(|| ProtocolError::BadValue(format!("error `{raw}`")))?;
                    let code = ErrorCode::parse(code)
                        .ok_or_else(|| ProtocolError::BadValue(format!("error code `{code}`")))?;
                    Some(CallError {
                        code,
                        message: message.to_string(),
                    })
                }
                None => None,
            };
            Ok(Message::Res {
                msg_id: num(fields, "msg_id")?,
                ok: match get(fields, "ok")? {
                    "true" => true,
                    "false" => false,
                    other => return Err(ProtocolError::BadValue(format!("ok=`{other}`"))),
                },
                result: get_opt(fields, "result").unwrap_or("").to_string(),
                error,
            })
        }
        "OPEN" => Ok(Message::Open {
            stream_id: num(fields, "stream_id")?,
            capability: get(fields, "capability")?.to_string(),
            args: pairs(get_opt(fields, "args").unwrap_or(""))?,
            credit: num(fields, "credit")?,
        }),
        "DATA" => Ok(Message::Data {
            stream_id: num(fields, "stream_id")?,
            seq: num(fields, "seq")?,
            payload: payload.to_vec(),
        }),
        "WINDOW" => Ok(Message::Window {
            stream_id: num(fields, "stream_id")?,
            credit: num(fields, "credit")?,
        }),
        "CLOSE" => Ok(Message::Close {
            stream_id: num(fields, "stream_id")?,
            reason: get_opt(fields, "reason").unwrap_or("").to_string(),
        }),
        "STATE" => {
            let mut workloads = Vec::new();
            let raw = get_opt(fields, "workloads").unwrap_or("");
            if !raw.is_empty() {
                for item in raw.split(',') {
                    let mut p = item.split(':');
                    let id = p
                        .next()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ProtocolError::BadValue("workload id".into()))?;
                    let state_raw = p
                        .next()
                        .ok_or_else(|| ProtocolError::BadValue("workload state".into()))?;
                    let state = WorkloadState::parse(state_raw)
                        .ok_or_else(|| ProtocolError::BadValue(format!("state `{state_raw}`")))?;
                    let generation = p
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| ProtocolError::BadValue("workload generation".into()))?;
                    let pid = p.next().and_then(|s| s.parse().ok());
                    let exit_code = p.next().and_then(|s| s.parse().ok());
                    let started_at = p.next().and_then(|s| s.parse().ok());
                    let cpu_millis = p.next().and_then(|s| s.parse().ok());
                    let memory_mib = p.next().and_then(|s| s.parse().ok());
                    workloads.push(Workload {
                        id: id.to_string(),
                        generation,
                        state,
                        pid,
                        exit_code,
                        started_at,
                        cpu_millis,
                        memory_mib,
                    });
                }
            }
            Ok(Message::State {
                conn_id: num(fields, "conn_id")?,
                generation: num(fields, "generation")?,
                workloads,
                capacity: Capacity {
                    cpu_millis: num(fields, "cpu_millis")?,
                    memory_mib: num(fields, "memory_mib")?,
                    disk_free_mib: num(fields, "disk_free_mib")?,
                },
                caps: list(fields, "caps")?,
            })
        }
        "METRICS" => Ok(Message::Metrics {
            epoch: num(fields, "epoch")?,
            cpu_pct: num(fields, "cpu")?,
            mem_mb: num(fields, "mem_mb")?,
            load1: num(fields, "load1")?,
        }),
        "ROUTES" => Ok(Message::Routes {
            rev: num(fields, "rev")?,
            entries: pairs(get_opt(fields, "entries").unwrap_or(""))?,
        }),
        other => Err(ProtocolError::BadFrame(format!("unknown tag `{other}`"))),
    }
}

fn hex_encode(id: &EventId) -> String {
    let mut out = String::with_capacity(32);
    for b in id {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_decode(raw: &str) -> Result<EventId, ProtocolError> {
    if raw.len() != 32 {
        return Err(ProtocolError::BadValue(format!(
            "event_id `{raw}` is not 32 hex characters"
        )));
    }
    let mut id = [0u8; 16];
    for (i, byte) in id.iter_mut().enumerate() {
        let pair = raw
            .get(i * 2..i * 2 + 2)
            .ok_or_else(|| ProtocolError::BadValue(format!("event_id `{raw}`")))?;
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| ProtocolError::BadValue(format!("event_id `{raw}`")))?;
    }
    Ok(id)
}

/// Parse `k:v,k:v` pairs.
fn pairs(raw: &str) -> Result<Vec<(String, String)>, ProtocolError> {
    let mut out = Vec::new();
    if raw.is_empty() {
        return Ok(out);
    }
    for item in raw.split(',') {
        let (k, v) = item
            .split_once(':')
            .ok_or_else(|| ProtocolError::BadValue(format!("pair `{item}` has no `:`")))?;
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: Message) -> Message {
        let bytes = m.encode().expect("encode");
        let (back, used) = Message::decode(&bytes).expect("decode");
        assert_eq!(used, bytes.len(), "decoder must consume the whole frame");
        assert_eq!(back, m);
        back
    }

    #[test]
    fn hello_roundtrips() {
        roundtrip(Message::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".into(),
            node_id: "n-7f3a".into(),
            caps: vec!["exec".into(), "metrics".into()],
        });
    }

    #[test]
    fn hello_ack_carries_optional_retry_hint() {
        roundtrip(Message::HelloAck {
            protocol: 1,
            lost_ms: 40_000,
            retry_after_ms: None,
        });
        roundtrip(Message::HelloAck {
            protocol: 1,
            lost_ms: 40_000,
            retry_after_ms: Some(5_000),
        });
    }

    #[test]
    fn watchdog_roundtrips() {
        roundtrip(Message::Watchdog { epoch: 7 });
    }

    #[test]
    fn request_and_response_roundtrip() {
        roundtrip(Message::Req {
            msg_id: 42,
            capability: "exec".into(),
            args: vec![
                ("cmd".into(), "ls -l".into()),
                ("cwd".into(), "/workspace".into()),
            ],
            event_id: Some([0xab; 16]),
        });
        roundtrip(Message::Res {
            msg_id: 42,
            ok: false,
            result: String::new(),
            error: Some(CallError {
                code: ErrorCode::Busy,
                message: "at capacity".into(),
            }),
        });
        roundtrip(Message::Res {
            msg_id: 43,
            ok: true,
            result: "done".into(),
            error: None,
        });
    }

    #[test]
    fn stream_frames_roundtrip() {
        roundtrip(Message::Open {
            stream_id: 3,
            capability: "port.forward".into(),
            args: vec![("port".into(), "8080".into())],
            credit: 65536,
        });
        roundtrip(Message::Window {
            stream_id: 3,
            credit: 1024,
        });
        roundtrip(Message::Close {
            stream_id: 3,
            reason: "eof".into(),
        });
    }

    #[test]
    fn data_frame_carries_raw_payload_after_the_header() {
        let payload = vec![0u8, 1, 2, 0xff, b'\n', b';', b'|', b'='];
        let m = Message::Data {
            stream_id: 9,
            seq: 1,
            payload: payload.clone(),
        };
        let bytes = m.encode().expect("encode");
        // The payload contains separators, which must survive untouched.
        let (back, used) = Message::decode(&bytes).expect("decode");
        assert_eq!(used, bytes.len());
        match back {
            Message::Data { payload: p, .. } => assert_eq!(p, payload),
            other => panic!("expected DATA, got {other:?}"),
        }
    }

    #[test]
    fn state_and_metrics_and_routes_roundtrip() {
        roundtrip(Message::State {
            conn_id: 12,
            generation: 4,
            workloads: vec![
                Workload {
                    id: "ci-1".into(),
                    generation: 4,
                    state: WorkloadState::Running,
                    pid: Some(1234),
                    exit_code: None,
                    started_at: Some(1_793_000_000),
                    cpu_millis: Some(2_500),
                    memory_mib: Some(512),
                },
                Workload {
                    id: "ci-0".into(),
                    generation: 3,
                    state: WorkloadState::Failed,
                    pid: None,
                    exit_code: Some(1),
                    started_at: None,
                    cpu_millis: None,
                    memory_mib: None,
                },
            ],
            capacity: Capacity {
                cpu_millis: 8000,
                memory_mib: 32768,
                disk_free_mib: 512_000,
            },
            caps: vec!["exec".into(), "container".into()],
        });
        roundtrip(Message::Metrics {
            epoch: 4,
            cpu_pct: 12.5,
            mem_mb: 4096,
            load1: 0.75,
        });
        roundtrip(Message::Routes {
            rev: 9,
            entries: vec![("cell-a".into(), "node-1".into())],
        });
    }

    #[test]
    fn decoder_reports_incomplete_before_a_full_frame_arrives() {
        let bytes = Message::Watchdog { epoch: 1 }.encode().unwrap();
        for cut in 0..bytes.len() {
            assert_eq!(
                Message::decode(&bytes[..cut]),
                Err(ProtocolError::Incomplete),
                "prefix of {cut} bytes should be incomplete"
            );
        }
    }

    #[test]
    fn decoder_resumes_from_a_buffer_holding_two_frames() {
        let a = Message::Watchdog { epoch: 1 }.encode().unwrap();
        let b = Message::Watchdog { epoch: 2 }.encode().unwrap();
        let mut buf = a.clone();
        buf.extend_from_slice(&b);

        let (first, used) = Message::decode(&buf).unwrap();
        assert_eq!(first, Message::Watchdog { epoch: 1 });
        let (second, used2) = Message::decode(&buf[used..]).unwrap();
        assert_eq!(second, Message::Watchdog { epoch: 2 });
        assert_eq!(used + used2, buf.len());
    }

    #[test]
    fn encoder_refuses_a_value_containing_a_separator() {
        let err = Message::Req {
            msg_id: 1,
            capability: "exec".into(),
            args: vec![("cmd".into(), "a;b".into())],
            event_id: None,
        }
        .encode()
        .unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidField(_)));

        let err = Message::Close {
            stream_id: 1,
            reason: "bad|reason".into(),
        }
        .encode()
        .unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidField(_)));
    }

    #[test]
    fn encoder_refuses_a_list_element_containing_a_list_separator() {
        let err = Message::Hello {
            protocol: 1,
            agent_version: "0.1.0".into(),
            node_id: "n".into(),
            caps: vec!["a,b".into()],
        }
        .encode()
        .unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidField(_)));
    }

    #[test]
    fn decoder_rejects_an_unknown_tag_and_a_bad_length() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(b"NOPE|");
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));

        let mut huge = Vec::new();
        huge.extend_from_slice(&(MAX_HEADER as u32 + 1).to_le_bytes());
        assert!(matches!(
            Message::decode(&huge),
            Err(ProtocolError::BadFrame(_))
        ));
    }

    #[test]
    fn decoder_rejects_malformed_fields() {
        // Field without `=`.
        let header = b"WATCHDOG|epoch";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));

        // Non-numeric epoch.
        let header = b"WATCHDOG|epoch=soon";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadValue(_))
        ));
    }

    #[test]
    fn a_hostile_payload_length_is_an_error_not_a_panic() {
        // A `len` far larger than the buffer must not overflow the end computation.
        let header = b"DATA|stream_id=1;seq=1;len=18446744073709551615";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));
    }

    #[test]
    fn a_payload_larger_than_the_cap_is_refused() {
        let header = b"DATA|stream_id=1;seq=1;len=1048577";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));
    }

    #[test]
    fn len_is_rejected_on_a_non_data_frame() {
        let header = b"WATCHDOG|epoch=1;len=5";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));
    }

    #[test]
    fn an_error_message_may_contain_a_colon() {
        // The wire form is `code:message:retryable`; the message is peeled from the right.
        let m = Message::Res {
            msg_id: 7,
            ok: false,
            result: String::new(),
            error: Some(CallError {
                code: ErrorCode::Busy,
                message: "at capacity: retry later".into(),
            }),
        };
        let bytes = m.encode().expect("encode");
        let (back, _) = Message::decode(&bytes).expect("decode");
        assert_eq!(back, m);
    }

    #[test]
    fn retryability_crosses_the_wire() {
        let m = Message::Res {
            msg_id: 7,
            ok: false,
            result: String::new(),
            error: Some(CallError {
                code: ErrorCode::Denied,
                message: "policy".into(),
            }),
        };
        let bytes = m.encode().expect("encode");
        let text = String::from_utf8_lossy(&bytes[4..]);
        assert!(
            text.contains("denied:policy:false"),
            "retryable must be on the wire: {text}"
        );
    }

    #[test]
    fn the_decoder_rejects_a_duplicate_field() {
        let header = b"WATCHDOG|epoch=1;epoch=2";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadFrame(_))
        ));
    }

    #[test]
    fn the_decoder_rejects_a_value_containing_a_separator() {
        // The encoder refuses these, so the decoder must not accept them either.
        let header = b"CLOSE|stream_id=1;reason=a=b";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(header);
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::InvalidField(_))
        ));
    }

    #[test]
    fn metrics_round_trip_exactly_for_awkward_floats() {
        for (cpu, load) in [(12345.678f32, 0.125f32), (0.0, 0.0), (99.99, 1.005)] {
            let m = Message::Metrics {
                epoch: 1,
                cpu_pct: cpu,
                mem_mb: 1,
                load1: load,
            };
            let bytes = m.encode().expect("encode");
            let (back, _) = Message::decode(&bytes).expect("decode");
            match back {
                Message::Metrics { cpu_pct, load1, .. } => {
                    assert_eq!(cpu_pct, cpu);
                    assert_eq!(load1, load);
                }
                other => panic!("expected METRICS, got {other:?}"),
            }
        }
    }

    #[test]
    fn error_codes_map_to_retryability() {
        assert!(ErrorCode::Busy.retryable());
        assert!(ErrorCode::Disconnected.retryable());
        assert!(ErrorCode::Timeout.retryable());
        assert!(ErrorCode::StorageUnavailable.retryable());
        assert!(!ErrorCode::Fenced.retryable());
        assert!(!ErrorCode::Denied.retryable());
        assert!(!ErrorCode::NotFound.retryable());
        assert!(!ErrorCode::UnknownCapability.retryable());

        for code in [
            ErrorCode::UnknownCapability,
            ErrorCode::Denied,
            ErrorCode::NotFound,
            ErrorCode::Busy,
            ErrorCode::Disconnected,
            ErrorCode::Fenced,
            ErrorCode::Timeout,
            ErrorCode::StorageUnavailable,
        ] {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        }
    }
}
