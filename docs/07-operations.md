# 07 — Operations

## Install

```bash
curl -fsSLO --proto '=https' https://releases.example/install.sh
curl -fsSLO --proto '=https' https://releases.example/install.sh.sig
minisign -Vm install.sh -P <release-key-fingerprint>
sh install.sh --token <enrolment-token>
```

The token is single-use, valid for 10 minutes and bound to a `node_id` the control plane has
already created. The script:

1. downloads the `cfbd` release for the host architecture, plus its signature and checksum;
2. verifies the checksum and the signature against the release key whose fingerprint is
   published in the repository, before anything is executed;
3. writes `/etc/cfbd/config.toml`;
4. installs the systemd unit;
5. starts the unit, and `cfbd` exchanges the token for its long-lived credential.

The script itself is fetched and verified before it runs, never piped straight into an
interpreter, and it is served from a release origin rather than from the Worker that serves the
dashboard: a script that writes the node's policy must not come from the origin whose reach that
policy bounds. `install.sh` writes `control_url`, `node_id`, `token_file`, `state_dir` and
`storage_url`; the `capabilities` block comes from a policy file the operator places on the
host, and the agent refuses to start if its signature does not verify.

Re-running the script is idempotent: on a host that already runs `cfbd` it upgrades the binary
in place and leaves the configuration and the credential untouched.

```toml
# /etc/cfbd/config.toml
control_url          = "wss://cfb.example/api/agent/ws"
node_id              = "n-7f3a"
token_file           = "/etc/cfbd/node.token"
state_dir            = "/var/lib/cfbd"
storage_url          = "s3://cfb-artifacts?endpoint=http://10.96.0.7:8333&region=us-east-1"
capabilities         = ["exec", "container", "port.forward", "metrics", "storage.proxy"]
lost_threshold_ms    = 40000
reconnect_max_ms     = 30000
metrics_interval_ms  = 3000
```

Environment overrides are `CFB_<KEY>` in upper case and take precedence over the file. Key
meanings and the `lost_threshold_ms` constraint are in [`02-agent.md`](02-agent.md).

## Upgrade

Two things upgrade on different clocks: the agent binary, and the workloads it supervises.

### Agent

`cfbd upgrade --to <version>` downloads the new binary, swaps it in and re-executes it under a
health-gated window. The new binary must report healthy within 60 s; otherwise the previous
binary is restored and re-executed, and the node stays on the version it started with. The
window is unconditional: a bad agent release removes the remote path to correct it.

Roll one node at a time.

### Workloads

Images are referenced by digest, so an upgrade is a new digest in the spec rather than a tag
moved underneath a running process. A spec change bumps `generation`; the agent starts the
workload at the new generation and stops the old one. Applying the same
`(workload_id, generation)` twice is a no-op, so a duplicate or retried push cannot start a
second copy.

| Workload | Behaviour on a spec change |
|---|---|
| Workers app | new instance starts, old instance stops; ~5 ms gap |
| Long-lived service | rolling restart, with the control plane draining the node first |

## Backup

| What | How | Where | RPO |
|---|---|---|---|
| D1 — desired state, jobs, audit | `wrangler d1 export` nightly | R2 `cfb-backups` | 24 h |
| Object data — `images`, `artifacts`, `cache`, `logs` | off-site replica | second site | 1 h |
| `celld-<app>` bucket, where celld runs | off-site replica | second site | 1 h |
| Node config — `/etc/cfbd`, the unit, workload specs | committed | git | per change |
| Secrets — node credentials, service tokens | offline password manager | — | — |

An alert fires if a backup has not run in 36 hours.

## Restore

```bash
wrangler d1 create cfb-restore
wrangler d1 import cfb-restore --file=cfb-2026-10-29.sql
# repoint the D1 binding, deploy cfb-manager, verify the node count
```

A celld fleet restores in one order:

1. Stop every celld node.
2. Restore the bucket.
3. Start one node, and verify it owns the cells it should.
4. Start the remaining nodes.

Never start a celld node against a partially restored bucket: half a bucket is two owners for
one cell, silent corruption rather than an outage.

Rehearse quarterly on a scratch host and record the elapsed time.

## Out-of-band access

The agent opens no inbound port, so every path in is deliberate.

| Failure | Path |
|---|---|
| A node is unhealthy | `cfbd` local API over `/run/cfbd/agent.sock`, or SSH over the overlay |
| The agent is wedged | `systemctl restart cfbd` |
| The control plane is down | `cfbd apply <spec.json>` locally, with a spec from git |
| The node is unreachable | provider console: IPMI, serial console, KVM |

The provider console credential location MUST be documented and tested annually.

## Capacity

Three numbers per resource: current, growth per month, and headroom in months. Each has a
written trigger; bytes per build are measured on the first day.

| Resource | Measure | Add capacity at |
|---|---|---|
| Storage bytes | per bucket | 70% of usable |
| CI capacity | oldest queued job age | > 10 min sustained |
| Node memory | agent-reported free | < 20% |
| Node count | per control plane | 200 nodes |

## Monitoring

Five signals, each with an alert threshold:

1. **Agent connections** — the expected count; alert below it.
2. **Workload failure rate** — counted as crash-loops, not as individual failures.
3. **Oldest queued job age** — the capacity signal.
4. **Storage free space and replication backlog.**
5. **Conditional-write probe** — only where celld runs.

The probe writes a probe key hourly with two concurrent `If-None-Match: *` PUTs and asserts
that exactly one succeeds and the other fails its precondition. A store that accepts both has
lost compare-and-swap, and the fleet will elect two owners for one cell. Success and silent
corruption are indistinguishable from outside: every write returns `200`, the bucket reads back
a plausible record, and no workload reports an error. The probe is the only signal that
separates them, which is why it is measured rather than inferred from lease behaviour.

## Failure modes

| Failure | Symptom | Action |
|---|---|---|
| Control link down | nodes show stale state; actions return `503 retryable` | check the connection, then the `SITE` tunnel; workloads keep running |
| Agent wedged | node reports but does not act on commands | restart the unit; children are reaped by `KillMode=mixed` |
| Workload crash-loop | node capacity falls | stop the workload, fix the image; the crash-loop budget is a hard stop, not a retry hint |
| Node disk full | new workloads refused, `disk_free_mib` at zero | expire `cache`, add a volume |
| Storage backend loses CAS | celld lease conflicts, or none at all | stop celld, roll back the storage version, audit the bucket |
| Cloudflare outage | dashboard unreachable | no action; the fleet keeps running |

The diagnostic question for every incident: *did a control-plane failure cause a data-plane
failure?* If yes, a plane boundary was crossed, and that is the defect to fix.
