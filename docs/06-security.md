# 06 — Security

The control plane is trusted to issue commands and not trusted with data. A payload host is
trusted with its own credentials and not trusted with any other host's; workload code is not
trusted in any case. Each boundary below is enforced by a component that does not depend on
the boundary above it.

## Trust boundaries

```
browser ──▶ Cloudflare Access ──▶ cfb-manager ──▶ cfbd ──▶ workload
            identity              control plane     capability   isolation
```

| Boundary | What crosses | Enforced by |
|---|---|---|
| browser → Cloudflare Access | session cookie, OIDC assertion | one Access application per hostname and role, one AUD per application; service tokens are static credentials with a configured lifetime (one year by default), rotated by hand |
| Cloudflare Access → `cfb-manager` | Access JWT | signature and AUD check in the Worker; the origin answers only through Access |
| `cfb-manager` → `cfbd` | RPC command over the connection | node-scoped credential at connect; agent-side capability policy; one `NodeDO` per node |
| `cfbd` → workload | workload definition, image digest, environment | the isolation level in [`03-workloads.md`](03-workloads.md) |
| workload → network | egress | per-slot nftables; the pull-through proxy is the only route |
| `cfbd` → storage | presign request, object read and write | bucket-scoped storage credential; `storage_url` |

Who holds what:

| Principal | Holds | Does not hold |
|---|---|---|
| browser | an Access session | any Cloudflare or storage credential |
| `cfb-manager` | the Cloudflare API token for the site; the enrolment authority | a bucket credential, an object, a payload credential |
| `cfbd` | its own long-lived node credential; the storage credential used to presign | another node's credential; the Cloudflare API token |
| workload | nothing | any route except the pull-through proxy |

No payload host accepts an inbound connection (R4). Every path is outbound from the agent or
a unix socket on the host, so the Worker is the only reachable surface.

## Scope of impact

Control of the Cloudflare account is control of the control plane for that site. Precisely: the
Worker holds a dedicated per-site Cloudflare API token, with tunnel and DNS permissions split,
and the enrolment authority. It can call any capability on any connected agent and can create
or delete the tunnels and DNS records that publish the site. It holds no bucket credential:
object access comes from a presign the agent performs, so the Worker's reach is the agent's
reach for the lifetime of that URL.

| Measure | Effect |
|---|---|
| Agent-side capability policy, owned by the agent | bounds which of a node's powers a Worker running foreign code can reach; it does not bound the powers themselves, because `exec`, `container`, `vm`, `workerd` and `port.forward` are root-equivalent on a node that declares them (see [`02-agent.md`](02-agent.md)) |
| Storage credential scoped per bucket, held by the agent | a leaked credential reaches one bucket; a Worker running foreign code reaches only what it can get signed, under the node's key prefix |
| Presign scope | the agent signs only keys under `/nodes/<node_id>/` and `/tenants/<org>/`, with a TTL of at most 300 s; a PUT presign carries `If-None-Match: *`; DELETE is not presignable |
| No overwrite on the `images` bucket | images are append-only from the Worker side, so a Worker running foreign code cannot swap the digest under a running workload |
| Hardware-key MFA on the Cloudflare account | account access needs a physical key, not a password and a session |
| Break-glass identity held offline | recovery when Access or the API token is misconfigured |
| Agents require the connection, not a public port | a Worker-only loss of integrity still needs the agent to be connected and willing |
| Audit-log webhook on mutations | tunnel, DNS and Access changes leave a record outside the account |

Residual risk: an account holder can drive every connected agent within its capability policy
and serve different static assets until the token is rotated. A node that declares `exec`,
`container`, `vm`, `workerd` or `port.forward` has delegated its root within that capability's
policy, and the policy is the whole of the restriction.

## Risk register

Each entry states a condition the deployment must hold against, and the measure that holds it.
The conditions are properties of a running system, not hypothetical adversaries: most of them
occur by misconfiguration, by an unmaintained dependency, or by a job that behaves unexpectedly.

### 1. Object store reachable without authentication

**Condition** — SeaweedFS ships with S3 authentication off by default. A gateway reachable over
a public hostname without an `s3.json` is a world-readable, world-writable bucket.

**Mitigation** — Write `s3.json` before the gateway is reachable. Never publish the S3 port on
a public hostname, and reach the gateway only from agents and runners over the private network.

### 2. Enrolment token handling

**Condition** — The enrolment token is the one credential a human handles, so it lands in shell
history, a chat message, or a terminal scrollback.

**Mitigation** — Expiry of 10 minutes (default), single use, bound to a pre-created `node_id`,
exchanged once for the long-lived node credential written `0600`. A spent token presented a
second time is an alert, not a retry. A new node lands in a `pending` group that reaches
nothing until an operator moves it.

### 3. Forwarded requests reaching unintended destinations

**Condition** — `env.SITE` reaches any address in the bound network, and the destination is a
runtime string. A route that builds the destination from request input forwards a request to a
destination the caller chose.

**Mitigation** — One `vpcFetch()` function with a static host and port table, and a lint rule
that `env.SITE.fetch` appears exactly once in the tree. Route handlers pass a table key, never
a URL.

### 4. Isolation boundary between jobs

**Condition** — CI runs code from arbitrary repositories. A container that crosses the
isolation boundary reaches the node's network, other jobs' caches, and the node credential.

**Mitigation** — A microVM for any code not written in-house. Firecracker's jailer with seccomp,
cgroups, chroot and a dedicated uid per VM; per-slot nftables; the VM's only route is the
node-local pull-through proxy on one port, which holds the storage credential the VM never
sees. Residual risk is a host kernel defect: patch the host, and do not run untrusted code on a
node that also holds a fleet-wide credential such as a `celld-<app>` bucket key.

### 5. Plaintext peer traffic

**Condition** — celld's peer path is plain HTTP inside an upgraded stream, so a peer link is
readable to anything on the path.

**Mitigation** — If celld is used, an encrypted overlay is mandatory: WireGuard or Headscale,
with DERP relays forwarding ciphertext they cannot read.

### 6. The relay

**Condition** — Any relay kept for a component the agent does not cover, such as celld's
listener or an S3 gateway, holds credentials and faces the network directly.

**Mitigation** — Keep the relay stateless, log every allow and deny decision, and fail closed
when the policy source is unreachable. A relay that cannot decide does not forward.

### 7. Artifact provenance

**Condition** — A tag can be repointed after review, and every dependency in the manager runs
with control-plane authority.

**Mitigation** — Images by digest, never by tag. The image bucket is write-once from the Worker
side. Keep the Worker's dependency list short: each package can reach the API token.

### 8. The self-updating daemon

**Condition** — `cloudflared` self-updates from `update.argotunnel.com` every 24 hours and
restarts: a scheduled outage on a host holding long-lived connections.

**Mitigation** — Pin the version, disable auto-update, and upgrade in a maintenance window with
the drain behaviour in [`07-operations.md`](07-operations.md).

### 9. Resource exhaustion on the manager

**Condition** — A job logging in a tight loop fills the SSE fanout and the Worker's memory,
which takes the dashboard down for every node on the site.

**Mitigation** — Rate-limit the log path per job, drop lines rather than buffer them, and let
the agent's ring buffer absorb the remainder. The limit is enforced in the Worker.

### 10. Port forwarding reaching beyond the node

**Condition** — `port.forward` exposes any local port the agent can reach: the container
socket, the site's S3 gateway, the agent's own socket, and `169.254.169.254` on a cloud host. A
control plane or dashboard session that has lost integrity then has a path into the site's
internal network.

**Mitigation** — `port.forward` requires a per-node destination allowlist. The agent refuses a
port that is not on it, refuses `169.254.0.0/16` and its own socket unconditionally, opens one
forward per allowlist entry, records the requesting `msg_id` and operator identity, and closes
every forward when the connection is fenced.

### 11. File paths resolving outside the job directory

**Condition** — `fs` names a job directory, but a key of `../../../etc/cfbd/node.token`, or a
symlink planted inside the directory, reads the node credential and turns a control-plane
failure into node-level control.

**Mitigation** — Every path is resolved with `openat2(2)` and `RESOLVE_BENEATH |
RESOLVE_NO_SYMLINKS`. A component containing `..`, an absolute path, or a symlink is refused
with `denied`. The operation runs as the job's uid, not the agent's, and the job directory is
mode `0700` owned by that uid.

## Multi-tenancy

The platform is single-tenant at the organisation level and not at the job level. Workloads
from different repositories are mutually untrusted and are isolated by microVM, not by a
tenant boundary in the control plane. There is no billing boundary, no enforced per-tenant
quota beyond the container runtime's cgroup limits, and no per-tenant key management. One
deployment serves one organisation; a second organisation is a second deployment with its own
Worker, buckets and account scope.

## Defaults to check before first boot

| Default | Effect | Fix |
|---|---|---|
| SeaweedFS S3 authentication off | object store reachable without authentication | write `s3.json` before the gateway is reachable |
| `cloudflared` auto-update on | restart every 24 hours at an unchosen version | pin the version, disable auto-update |
| celld peers plaintext | peer traffic readable on the path | encrypted overlay before celld starts |
| Workers VPC in beta, unpriced | API and pricing change under the deployment | keep the self-hosted fallback exercised |
| agent token in shell history | credential disclosure | exchange once, write `0600` |
| secrets in `wrangler.jsonc` | credentials committed with the source | `wrangler secret put` |

The installer refuses to start in an insecure configuration, and the dashboard reads these
defaults back in a posture panel so a drift is visible before the first job runs.
