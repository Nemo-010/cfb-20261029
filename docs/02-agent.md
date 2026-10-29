# 02 — cfbd, the node agent

`cfbd` is one static binary per host. It is the connection, the RPC broker, the supervisor,
the metrics source and the local API. A node needs this binary, a container runtime or
`workerd` if it runs those workloads, and outbound reachability to its storage endpoint; it
needs no other daemon.

Static linking is a build goal: release builds use `-C target-feature=+crt-static`, so a node
needs the kernel and its device files, not a libc package or an interpreter.

## Process model

```
cfbd
├── tunnel        one outbound WebSocket over TLS to cfb-manager
├── broker        REQ/RES correlation, OPEN/DATA/WINDOW/CLOSE streams, WATCHDOG
├── supervisor    child process per workload, own cgroup, restart policy, log ring buffer
├── api           /run/cfbd/agent.sock — status, ps, logs, exec, apply, doctor
└── metrics       /proc sampling, 3 s push over the connection
```

**No listening TCP port on any external interface.** Every path is either an outbound
connection initiated by the agent or a unix socket on the host. A node behind NAT with no
inbound rule is the normal deployment.

## The connection

| Property | Rule |
|---|---|
| Direction | agent-initiated; the node needs no public address and no certificate for a hostname |
| Shape | one dial, held open, reconnected forever |
| Transport | WebSocket over TLS to `control_url` |
| Auth | node token, exchanged at enrolment for a long-lived credential; mTLS optional where a private CA exists |
| Reconnect | 1 s doubling to a 30 s cap, with jitter |
| Fencing | monotonic `conn_id` per connection |

Reconnect is not restart: listeners, child processes and their logs survive a dropped link.
The agent does not restart workloads because its control link blipped.

Fencing makes single ownership enforceable. Every connection carries a monotonically
increasing `conn_id`, claimed in the same write that records the connection. A socket whose
`conn_id` is older than the recorded one is evicted rather than trusted, and any command
arriving on it is refused with `fenced`.

## Wire protocol

A frame is a `u32` little-endian length followed by a UTF-8 body:

```
frame  := length:u32le   body[length]
body   := tag "|" field ( ";" field )*
field  := key "=" value
```

The body is UTF-8 text. Field values MUST NOT contain the field separators — `|`, `;`, `=`,
and `,` or `:` inside list values — so no escaping and no quoting exist anywhere in the
protocol. A value containing a separator is a protocol error.

Binary payloads travel only in `DATA` frames, which are length-delimited: the text header
carries `len` and exactly `len` raw bytes follow the body.

| Message | Direction | Fields | Purpose |
|---|---|---|---|
| `HELLO` | agent→cp | `protocol`, `agent_version`, `node_id`, `caps` | first frame after connect |
| `HELLO_ACK` | cp→agent | `protocol`, `lost_ms`, `retry_after_ms`? | session accepted |
| `WATCHDOG` | both | `epoch` | primary liveness signal |
| `REQ` | both | `msg_id`, `capability`, `args` | capability call |
| `RES` | both | `msg_id`, `ok`, `result`, `error` | reply, possibly out of order |
| `OPEN` | both | `stream_id`, `capability`, `args`, `credit` | open a stream |
| `DATA` | both | `stream_id`, `seq`, `len` + payload | stream bytes |
| `WINDOW` | both | `stream_id`, `credit` | grant credit |
| `CLOSE` | both | `stream_id`, `reason` | end a stream |
| `STATE` | agent→cp | `conn_id`, `generation`, `workloads`, `capacity`, `caps` | reconciliation input |
| `METRICS` | agent→cp | `epoch`, `cpu`, `mem_mb`, `load1` | 3 s push |
| `ROUTES` | cp→agent | `rev`, `entries` | cell→node table |

**`HELLO` / `HELLO_ACK`.** Version negotiation happens on the first exchange. The control
plane answers with the `protocol` it will speak, `lost_ms` — the threshold after which it
judges this node lost, default 40000 — and an optional `retry_after_ms` used to shed
reconnecting agents. `lost_ms` MUST exceed `reconnect_max_ms`, else the agent closes the
session.

**`WATCHDOG`.** In-band keepalive carrying the epoch. Absence of watchdog frames, not socket
close, drives the transition from healthy to `unknown` to lost. A broker disconnect alone
MUST NOT trigger re-placement: a node with a warm workload and a bad path to the edge is
still running that workload.

**`REQ` / `RES`.** A call names a capability and carries `args` as `k:v,k:v`; `RES` carries
`ok`, `result` and `error` as `code:message:retryable`. Correlation is by `msg_id`, replies
may be out of order, and a caller awaits a specific id. When the connection drops, every
outstanding waiter fails immediately with `disconnected`.

**`OPEN` / `DATA` / `WINDOW` / `CLOSE`.** One connection carries N concurrent streams — log
tails, exec sessions, port forwards, file transfers. Flow control is credit-based: `OPEN` and
`WINDOW` state how many bytes the peer may send, and a sender that exhausts its credit waits.
`seq` is monotonic per stream, so a consumer detects loss. Streams do not survive a
reconnect; the consumer re-opens with a cursor and the agent replays from its ring buffer.

**`STATE` / `METRICS` / `ROUTES`.** `STATE` is the full node and workload report, sent on
connect, on change and every 15 s. `METRICS` is CPU, memory and load every 3 s. Different
messages, different cadences; neither replaces the other. `ROUTES` is the cell→node table
tagged with `rev`; a push not newer than the cached rev is ignored.

## State report and reconciliation

```
STATE  conn_id, generation, workloads[], capacity{}, caps[]
  workload = { id, generation, state, pid?, exit_code?, started_at, cpu_millis, memory_mib }
  capacity = { cpu_millis, memory_mib, disk_free_mib }
```

`generation` is the reconciliation key. The control plane compares the generation it wants
against the one reported and pushes a spec only when the agent is behind. Applying the same
`(workload_id, generation)` twice is a no-op on the agent, so a duplicate or retried push
cannot start a second copy.

On reconnect the agent sends a complete report and the control plane diffs from scratch. That
is the recovery path, and it is why the report is full rather than incremental. Without a
control plane the agent keeps reconciling against its last known desired state.

## Error codes

| Code | Meaning | Retryable |
|---|---|---|
| `unknown_capability` | capability not declared by the agent | no |
| `denied` | local policy on the node refuses the call | no |
| `not_found` | workload or stream id unknown | no |
| `busy` | at capacity; retry with backoff | yes |
| `disconnected` | connection lost mid-call | yes |
| `fenced` | a newer connection owns this node | no |
| `timeout` | capability exceeded its deadline | yes |
| `storage_unavailable` | presign or upload failed | yes |

## Capabilities

Declared at enrolment, disableable locally, and checked on both ends: the control plane
refuses to call one it has not seen declared, and the agent refuses one that local policy
removed. A control plane that has lost integrity cannot grant a node a capability the node
does not have. It can use every capability the node does declare, so the declaration states
how far the node trusts the control plane, not whether it trusts it.

| Capability | Operations | Requires | Privilege |
|---|---|---|---|
| `exec` | run a command, stream stdout/stderr, return the exit code | nothing beyond the agent | root-equivalent |
| `container` | `start`, `stop`, `rm` from an image digest | Docker Engine API: `/var/run/docker.sock`, then `$XDG_RUNTIME_DIR/podman/podman.sock`, then `/run/podman/podman.sock`, then a systemd transient unit, then fork/exec plus cgroup v2 | root-equivalent |
| `vm` | `start`, `stop` a microVM | `/dev/kvm`, present and openable | root-equivalent |
| `workerd` | `start`, `stop` a Workers bundle | `workerd` binary on the host | root-equivalent |
| `port.forward` | expose a local port through the connection | a per-node destination allowlist; the agent refuses a port not on it, and refuses `169.254.0.0/16` and its own socket unconditionally | root-equivalent |
| `fs` | `read`, `write` under one job directory | `openat2(2)` with `RESOLVE_BENEATH` and `RESOLVE_NO_SYMLINKS`; the operation runs as the job's uid | per-job uid |
| `metrics` | CPU, memory, disk, load, per-workload usage | `/proc` | unprivileged |
| `storage.proxy` | presign a PUT or GET under the node's key prefix | the node's storage credential and `storage_url` | bucket-scoped |

The declared value is the family name; the operations column is what the family covers. The
privilege column states what a node gives up by declaring it: `exec`, `container`, `vm`,
`workerd` and `port.forward` run as the agent's user, which is root on a node that needs
`/dev/kvm`, the Docker socket or nftables. Declaring one of them delegates that power to the
control plane within the capability's policy, and the declaration is the whole of the
restriction.

Probes are measured, not guessed: a probe that cannot reach its interface reports `unknown`
rather than `no`. The egress probes are TCP connects on 443 and 7844; a QUIC path is proven by
the tunnel, not by `doctor`.

## Supervision

| Aspect | Behaviour |
|---|---|
| Unit | one child process per workload, in its own cgroup |
| Policies | `never`; `on-failure` (default); `always` |
| Restart backoff | 100 ms doubling to a 60 s cap; a run of 10 s or longer resets it to 100 ms |
| Crash loop | past its crash-loop budget a workload is marked `failed` and not retried until the workload spec changes |
| Logs | captured to a ring buffer, shipped as streams; the last lines stay resident so a reconnecting reader catches up |
| Shutdown | `SIGTERM`, then a grace period, then `SIGKILL`, in that order |
| Readiness | one `READY=1` line on stdout, s6 `notifywhenup` semantics |
| Lease | renewal only after readiness |
| Descriptors | the agent holds broker and listener fds across payload restarts |

Infinite restart is how a broken image consumes a node, so the budget is a hard stop.
Readiness gates the lease: a process that has started is not a process that can serve.

## Placement

Which node runs a workload is a control-plane decision made from declared capabilities and
reported capacity; see [`03-workloads.md`](03-workloads.md) and
[`05-control-plane.md`](05-control-plane.md).

## Local API

A unix socket at `/run/cfbd/agent.sock`, for an operator on the box and for nothing else. It
is not proxied through the connection and not reachable off-host.

| Subcommand | Output |
|---|---|
| `cfbd status` | node id, connection state, `conn_id`, workload count |
| `cfbd ps` | running workloads with state, pid and resource use |
| `cfbd logs <id>` | tail of the ring buffer, following |
| `cfbd exec <id> -- <cmd>` | command inside a workload |
| `cfbd apply <spec.json>` | apply desired state locally, bypassing the control plane |
| `cfbd doctor` | capability probe (KVM, container socket, egress, cores, memory) |

`doctor` must answer with the control plane unreachable: it is the first command run on a
misbehaving node, and a diagnostic that needs the broken path has none.

## Enrolment

| Step | Actor | Action |
|---|---|---|
| 1 | control plane | creates the node, returns a single-use token valid for 10 minutes, bound to the pre-created `node_id` |
| 2 | operator | runs the installer on the host with that token |
| 3 | installer | downloads `cfbd`, verifies its signature, writes `/etc/cfbd/config.toml`, installs the systemd unit, starts it |
| 4 | `cfbd` | exchanges the token for a long-lived credential written `0600`, dials, declares capabilities in `HELLO`, sends the first `STATE` |

The token is the only credential a human handles: exchanged once, never displayed again.

## Config

`/etc/cfbd/config.toml`. Environment overrides are `CFB_<KEY>` in upper case
(`CFB_CONTROL_URL`, `CFB_NODE_ID`, …) and take precedence over the file.

| Key | Meaning | Value |
|---|---|---|
| `control_url` | dial target for the connection | `wss://cfb.example/api/agent/ws` |
| `node_id` | node identity, pre-created at enrolment | `n-7f3a` |
| `token_file` | long-lived credential, mode `0600` | `/etc/cfbd/node.token` |
| `state_dir` | desired state, log buffers, workload metadata | `/var/lib/cfbd` |
| `storage_url` | S3 endpoint and bucket used by `storage.proxy` | `s3://cfb-artifacts?endpoint=http://10.96.0.7:8333&region=us-east-1` |
| `capabilities` | declared set, minus anything disabled locally | `["exec", "container", "port.forward", "metrics", "storage.proxy"]` |
| `lost_threshold_ms` | threshold the control plane must advertise | `40000` |
| `reconnect_max_ms` | cap on connection backoff | `30000` |
| `metrics_interval_ms` | metrics push interval | `3000` |

A configuration where `lost_threshold_ms` is not greater than `reconnect_max_ms` is refused
at startup: the two sides would disagree about when the node is lost.

## Failure behaviour

| Failure | Effect |
|---|---|
| Link down | workloads keep running; state is buffered locally; the dashboard shows `unknown` after `lost_ms`, then lost |
| Link flaps | reconnect with jittered backoff; no workload restart; the old connection is fenced |
| Agent crash | systemd restarts the unit; children are reaped; workloads return per their restart policy |
| Agent upgrade | the new binary runs in a health-gated window; if it does not report healthy, the old binary is restored and re-executed |
| Reboot | workloads restart per policy; `always` returns, `never` does not |
| Disk full | the agent refuses new workloads, reports zero `disk_free_mib`, and the control plane marks the node `disk_pressure`; existing workloads keep running |

## Implementation status

The crate at `agent/` implements the frame codec, config loading, the `doctor` probes and the
supervision primitives — restart backoff, readiness gating and the crash-loop reset. It does
not implement transport, the live connection, or the local API: `cfbd run` validates
configuration, probes the host and exits, and `cfbd doctor` prints the capability line.
