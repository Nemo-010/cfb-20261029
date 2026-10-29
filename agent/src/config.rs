//! Configuration loading for `cfbd`.
//!
//! The file format is a strict subset of TOML: `key = value` lines, `#` comments, quoted
//! strings, arrays of quoted strings, and unsigned integers. Anything else is rejected with
//! the line number, because a configuration file that half-parses is worse than one that
//! refuses to start.

use std::fmt;
use std::path::Path;

/// Capability families an agent may declare. A family covers its operations: `container`
/// covers `container.start`, `container.stop` and `container.rm`.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "exec",
    "container",
    "vm",
    "workerd",
    "port.forward",
    "fs",
    "metrics",
    "storage.proxy",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub control_url: String,
    pub node_id: String,
    pub token_file: String,
    pub state_dir: String,
    pub storage_url: String,
    pub capabilities: Vec<String>,
    pub lost_threshold_ms: u64,
    pub reconnect_max_ms: u64,
    pub metrics_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            control_url: String::new(),
            node_id: String::new(),
            token_file: String::from("/etc/cfbd/node.token"),
            state_dir: String::from("/var/lib/cfbd"),
            storage_url: String::new(),
            capabilities: Vec::new(),
            lost_threshold_ms: 40_000,
            reconnect_max_ms: 30_000,
            metrics_interval_ms: 3_000,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError {
    line: Option<usize>,
    detail: String,
}

impl ConfigError {
    fn at(line: usize, detail: impl Into<String>) -> Self {
        ConfigError {
            line: Some(line),
            detail: detail.into(),
        }
    }

    fn plain(detail: impl Into<String>) -> Self {
        ConfigError {
            line: None,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(n) => write!(f, "line {n}: {}", self.detail),
            None => write!(f, "{}", self.detail),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Load the file if it exists, then apply environment overrides.
    ///
    /// A missing file is not an error: defaults plus `CFB_*` variables are a valid source.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(ConfigError::plain(format!("{}: {e}", path.display()))),
        };
        cfg.apply_env(&|k| std::env::var(k).ok())?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse the file body. Unknown keys and malformed values are errors.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let mut cfg = Config::default();
        let mut seen: Vec<String> = Vec::new();

        for (idx, raw_line) in text.lines().enumerate() {
            let n = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                return Err(ConfigError::at(n, "sections are not supported"));
            }
            let (key, raw_value) = line
                .split_once('=')
                .ok_or_else(|| ConfigError::at(n, "expected `key = value`"))?;
            let key = key.trim();
            if !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(ConfigError::at(n, format!("invalid key `{key}`")));
            }
            if seen.iter().any(|k| k == key) {
                return Err(ConfigError::at(n, format!("duplicate key `{key}`")));
            }
            seen.push(key.to_string());

            cfg.set(n, key, raw_value.trim())?;
        }

        Ok(cfg)
    }

    fn set(&mut self, line: usize, key: &str, raw: &str) -> Result<(), ConfigError> {
        match key {
            "control_url" => self.control_url = parse_string(line, raw)?,
            "node_id" => self.node_id = parse_string(line, raw)?,
            "token_file" => self.token_file = parse_string(line, raw)?,
            "state_dir" => self.state_dir = parse_string(line, raw)?,
            "storage_url" => self.storage_url = parse_string(line, raw)?,
            "capabilities" => self.capabilities = parse_array(line, raw)?,
            "lost_threshold_ms" => self.lost_threshold_ms = parse_int(line, raw)?,
            "reconnect_max_ms" => self.reconnect_max_ms = parse_int(line, raw)?,
            "metrics_interval_ms" => self.metrics_interval_ms = parse_int(line, raw)?,
            other => return Err(ConfigError::at(line, format!("unknown key `{other}`"))),
        }
        Ok(())
    }

    /// Environment variables override the file. `CFB_<KEY>` in upper case.
    ///
    /// A variable that is present but unparseable is an error, not a silent fallback: an
    /// operator who typed `CFB_RECONNECT_MAX_MS=30s` must be told, not left with the default.
    pub fn apply_env(&mut self, get: &dyn Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        if let Some(v) = get("CFB_CONTROL_URL") {
            self.control_url = v;
        }
        if let Some(v) = get("CFB_NODE_ID") {
            self.node_id = v;
        }
        if let Some(v) = get("CFB_TOKEN_FILE") {
            self.token_file = v;
        }
        if let Some(v) = get("CFB_STATE_DIR") {
            self.state_dir = v;
        }
        if let Some(v) = get("CFB_STORAGE_URL") {
            self.storage_url = v;
        }
        if let Some(v) = get("CFB_CAPABILITIES") {
            self.capabilities = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
        }
        for (key, field) in [
            ("CFB_LOST_THRESHOLD_MS", &mut self.lost_threshold_ms),
            ("CFB_RECONNECT_MAX_MS", &mut self.reconnect_max_ms),
            ("CFB_METRICS_INTERVAL_MS", &mut self.metrics_interval_ms),
        ] {
            if let Some(v) = get(key) {
                *field = v
                    .parse()
                    .map_err(|_| ConfigError::plain(format!("{key}=`{v}` is not an integer")))?;
            }
        }
        Ok(())
    }

    /// Rules that make the rest of the agent's assumptions true.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.node_id.trim().is_empty() {
            return Err(ConfigError::plain("node_id is empty"));
        }
        if !(self.control_url.starts_with("ws://") || self.control_url.starts_with("wss://")) {
            return Err(ConfigError::plain(
                "control_url must be a ws:// or wss:// URL",
            ));
        }
        if self.token_file.trim().is_empty() {
            return Err(ConfigError::plain("token_file is empty"));
        }
        if self.state_dir.trim().is_empty() {
            return Err(ConfigError::plain("state_dir is empty"));
        }
        if !self.storage_url.is_empty() && !self.storage_url.starts_with("s3://") {
            return Err(ConfigError::plain(
                "storage_url must be s3://<bucket>?endpoint=…&region=…",
            ));
        }
        for cap in &self.capabilities {
            if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
                return Err(ConfigError::plain(format!(
                    "unknown capability `{cap}`; known: {}",
                    KNOWN_CAPABILITIES.join(", ")
                )));
            }
        }
        if self.metrics_interval_ms == 0 {
            return Err(ConfigError::plain("metrics_interval_ms must be positive"));
        }
        if self.lost_threshold_ms <= self.reconnect_max_ms {
            return Err(ConfigError::plain(format!(
                "lost_threshold_ms ({}) must exceed reconnect_max_ms ({})",
                self.lost_threshold_ms, self.reconnect_max_ms
            )));
        }
        Ok(())
    }
}

/// Remove a `#` comment, ignoring `#` inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' if in_str => escaped = true,
            b'"' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_string(line: usize, raw: &str) -> Result<String, ConfigError> {
    let t = raw.trim();
    if t.len() < 2 || !t.starts_with('"') || !t.ends_with('"') {
        return Err(ConfigError::at(line, "expected a quoted string"));
    }
    let inner = &t[1..t.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    return Err(ConfigError::at(
                        line,
                        format!("unsupported escape `\\{other}`"),
                    ))
                }
                None => return Err(ConfigError::at(line, "trailing backslash")),
            }
        } else if c == '"' {
            return Err(ConfigError::at(line, "unescaped quote inside string"));
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn parse_array(line: usize, raw: &str) -> Result<Vec<String>, ConfigError> {
    let t = raw.trim();
    if !t.starts_with('[') || !t.ends_with(']') {
        return Err(ConfigError::at(line, "expected an array"));
    }
    let inner = &t[1..t.len() - 1];
    let mut out = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        out.push(parse_string(line, p)?);
    }
    Ok(out)
}

fn parse_int(line: usize, raw: &str) -> Result<u64, ConfigError> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| ConfigError::at(line, format!("expected an integer, got `{}`", raw.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# node identity
control_url = "wss://cfb.example/api/agent/ws"
node_id     = "n-7f3a"
storage_url = "s3://cfb-artifacts?endpoint=http://10.96.0.7:8333&region=us-east-1"
capabilities = ["exec", "container", "metrics"]
lost_threshold_ms   = 40000
reconnect_max_ms    = 30000
"#;

    #[test]
    fn parses_every_key() {
        let c = Config::parse(SAMPLE).expect("parse");
        assert_eq!(c.control_url, "wss://cfb.example/api/agent/ws");
        assert_eq!(c.node_id, "n-7f3a");
        assert_eq!(c.capabilities, vec!["exec", "container", "metrics"]);
        assert_eq!(c.lost_threshold_ms, 40_000);
        assert_eq!(c.reconnect_max_ms, 30_000);
        // Untouched keys keep their defaults.
        assert_eq!(c.metrics_interval_ms, 3_000);
        assert_eq!(c.state_dir, "/var/lib/cfbd");
    }

    #[test]
    fn keeps_equals_signs_inside_quoted_values() {
        let c = Config::parse(SAMPLE).expect("parse");
        assert!(c.storage_url.contains("endpoint=http://10.96.0.7:8333"));
        assert!(c.storage_url.ends_with("region=us-east-1"));
    }

    #[test]
    fn strips_inline_comments_outside_quotes() {
        let text = r#"node_id = "n-1"   # the id
control_url = "wss://h/x" # trailing
"#;
        let c = Config::parse(text).expect("parse");
        assert_eq!(c.node_id, "n-1");
        assert_eq!(c.control_url, "wss://h/x");
    }

    #[test]
    fn keeps_hash_inside_quoted_value() {
        let c = Config::parse("node_id = \"n#1\"\n").expect("parse");
        assert_eq!(c.node_id, "n#1");
    }

    #[test]
    fn rejects_unknown_key_with_line_number() {
        let err = Config::parse("node_id = \"n\"\nbogus = 1\n").unwrap_err();
        assert_eq!(err.line, Some(2));
        assert!(err.to_string().contains("unknown key"));
    }

    #[test]
    fn rejects_duplicate_key() {
        let err = Config::parse("node_id = \"a\"\nnode_id = \"b\"\n").unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_sections_and_malformed_lines() {
        assert!(Config::parse("[agent]\n").is_err());
        assert!(Config::parse("node_id\n").is_err());
        assert!(Config::parse("node_id = n-1\n").is_err());
        assert!(Config::parse("lost_threshold_ms = soon\n").is_err());
    }

    #[test]
    fn env_overrides_file() {
        let mut c = Config::parse(SAMPLE).expect("parse");
        c.apply_env(&|k| match k {
            "CFB_NODE_ID" => Some("n-env".into()),
            "CFB_CAPABILITIES" => Some("exec, metrics".into()),
            "CFB_RECONNECT_MAX_MS" => Some("5000".into()),
            _ => None,
        })
        .expect("env");
        assert_eq!(c.node_id, "n-env");
        assert_eq!(c.capabilities, vec!["exec", "metrics"]);
        assert_eq!(c.reconnect_max_ms, 5_000);
    }

    #[test]
    fn a_malformed_numeric_override_is_an_error() {
        let mut c = Config::default();
        let err = c
            .apply_env(&|k| match k {
                "CFB_RECONNECT_MAX_MS" => Some("30s".into()),
                _ => None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("CFB_RECONNECT_MAX_MS"));
    }

    #[test]
    fn validate_rejects_an_unknown_capability() {
        let c = Config {
            node_id: "n-1".into(),
            control_url: "wss://h/x".into(),
            capabilities: vec!["exce".into()],
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("unknown capability"));
    }

    #[test]
    fn validate_rejects_a_storage_url_that_is_not_s3() {
        let c = Config {
            node_id: "n-1".into(),
            control_url: "wss://h/x".into(),
            storage_url: "not-a-url".into(),
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("s3://"));
    }

    #[test]
    fn validate_rejects_an_empty_state_dir() {
        let c = Config {
            node_id: "n-1".into(),
            control_url: "wss://h/x".into(),
            state_dir: String::new(),
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_enforces_the_lost_threshold_invariant() {
        let mut c = Config {
            node_id: "n-1".into(),
            control_url: "wss://h/x".into(),
            ..Config::default()
        };
        assert!(c.validate().is_ok());

        c.reconnect_max_ms = c.lost_threshold_ms;
        assert!(c.validate().is_err());

        c.reconnect_max_ms = 30_000;
        c.node_id = String::new();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_a_non_websocket_url() {
        let c = Config {
            node_id: "n-1".into(),
            control_url: "https://h/x".into(),
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("ws://"));
    }

    #[test]
    fn missing_file_falls_back_to_defaults_and_env() {
        let c = Config::load(Path::new("/nonexistent/cfbd/config.toml"));
        // load() applies the real environment; it must not panic and must either succeed
        // or fail only on validation of the default (empty) node id.
        match c {
            Ok(cfg) => assert!(!cfg.node_id.is_empty()),
            Err(e) => assert!(e.to_string().contains("node_id")),
        }
    }
}
