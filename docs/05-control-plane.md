# 05 — Control plane

`cfb-manager` is one Worker: static assets for the SPA, `/api/*` on Hono, one `NodeDO` per node,
D1 for desired state, R2 for logs, and the `vpc_networks` binding `SITE` for node access.

## Shape

```
cfb-manager (one Worker)
├── static assets      dist/ — SPA, no SSR
├── /api/*             Hono router, JSON and SSE
├── NodeDO             one per node: conn_id, sockets, streams, leases
├── D1                 nodes, workloads, jobs, audit
├── R2 (LOGS)          build and job logs
└── vpc_networks SITE  reaches node-local services with no public hostname
```

One deployable: D1, R2 and the Durable Object namespace are bindings in one configuration. The
Worker bundle limit is 64 MiB and the request body limit is 100 MB; the API carries control
messages only.

## Why the manager runs on Cloudflare

Control-plane traffic is small requests, globally distributed, low CPU, and must be served while
the fleet is not. A manager hosted on the hardware it manages is unreachable in the failure it
exists to report. Workers terminate at the edge, bill per request and scale to zero between
requests. The control plane therefore depends on Cloudflare; the data plane does not, because
`cfbd` and the payloads run on the operator's hosts and continue through an outage.

## Why a SPA and not SSR

Static asset requests are free and unlimited (Cloudflare pricing, 2026-10-29), so the dashboard
costs nothing at any request rate. Without SSR there is no Node compatibility layer in the
Worker, no renderer inside the 64 MiB bundle, and no server path to keep in step with the API.
The dashboard is an authenticated operator console — login, node list, workload forms, log
viewer — behind Access, with no anonymous first paint.

## Desired state

D1 holds intent. The node holds reality. The two are reconciled, never merged: no table is a
copy of agent state, and the agent keeps no copy of desired state.

```sql
CREATE TABLE nodes (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  capabilities TEXT NOT NULL,      -- JSON array
  labels       TEXT NOT NULL,      -- JSON array of strings
  state        TEXT NOT NULL,      -- pending|ready|stale|draining|revoked
  last_seen    INTEGER,
  conn_id      INTEGER
);

CREATE TABLE workloads (
  id           TEXT PRIMARY KEY,
  node_id      TEXT,
  spec         TEXT NOT NULL,      -- JSON: image, cpu, memory, mounts, env, restart
  state        TEXT NOT NULL,      -- pending|starting|running|succeeded|failed|stopped
  generation   INTEGER NOT NULL,   -- bumped on every spec change
  updated_at   INTEGER
);

CREATE TABLE jobs (
  id            TEXT PRIMARY KEY,
  workload_id   TEXT,
  idempotency   TEXT UNIQUE,       -- submit key
  exit_code     INTEGER,
  started_at    INTEGER,
  finished_at   INTEGER
);

CREATE TABLE audit (
  at      INTEGER,
  actor   TEXT,
  action  TEXT,
  target  TEXT,
  outcome TEXT
);
```

`generation` carries the reconciliation. It is an integer on the workload row, incremented by
the write that stores a spec change, and the agent reports the value it is running. A lower
report means the agent is behind and the spec is pushed again. Applying the same
`(workload_id, generation)` twice is a no-op on the agent, so a duplicate push cannot start a
second copy.

## The reconcile loop

```
every 15 s, for each node with an open connection:
  desired  = SELECT id, spec, generation FROM workloads
             WHERE node_id = ? AND state <> 'stopped'
  reported = node.state_report          # last full report from the agent
  for each w in desired:
    if w missing from reported or reported[w].generation < w.generation:
      push(w.spec, w.generation)
  for each w in reported not in desired:
    stop(w.id)                          # no-op if already stopping
```

Push, not pull. The agent holds a live connection, so it is told what changed instead of polling
D1. The 15 s interval only catches a push missed while the DO was cold or the connection was
re-establishing; it is not the delivery path. On reconnect the agent sends a full state report
and the control plane diffs from scratch, so the report must be complete. Apply is idempotent in
both directions, so retries need no deduplication.

## Placement

```ts
function place(w: Workload, nodes: Node[]): Node | null {
  const candidates = nodes.filter(n =>
    n.state === "ready" &&
    n.capabilities.includes(w.requiredCapability) &&
    n.freeMemoryMiB >= w.memoryMiB &&
    w.labels.every(l => n.labels.includes(l))
  );
  return candidates.sort((a, b) => b.freeMemoryMiB - a.freeMemoryMiB)[0] ?? null;
}
```

Filter on readiness, capability, free memory and labels; sort by free memory; take the first.
`state`, `capabilities` and `freeMemoryMiB` come from the node's last state report, held live in
its `NodeDO`; D1 carries a copy for the dashboard list and is never the placement input. Free
memory is a 3 s observation, not a reservation. This holds below a few hundred nodes.

It stops being sufficient for relations between workloads — gang scheduling, accelerator classes,
anti-affinity, preemption; see [`03-workloads.md`](03-workloads.md).

## API routes

```
GET    /api/v1/nodes                     list with health
POST   /api/v1/nodes/enroll              mint a one-time token
POST   /api/v1/nodes/{id}/drain          stop accepting new workloads
POST   /api/v1/nodes/{id}/revoke         revoke credential

GET    /api/v1/workloads                 list
PUT    /api/v1/workloads/{id}            upsert spec (idempotent, bumps generation)
DELETE /api/v1/workloads/{id}            stop and remove

POST   /api/v1/jobs                      submit one-shot (Idempotency-Key required)
GET    /api/v1/jobs/{id}                 status
GET    /api/v1/jobs/{id}/logs            SSE, cursor-based
POST   /api/v1/jobs/{id}/cancel          cancel

POST   /api/v1/storage/presign           forwarded to the node, which signs under its prefix
GET    /api/v1/audit                     append-only
```

1. **No request waits for a workload operation.** A submit writes the job row, pushes to the
   node and returns `202`; status is a separate read.
2. **Every mutation takes an idempotency key.** The VPC path has no stream resumption, so a
   retry after a timeout is normal traffic rather than an error.
3. **Errors carry `code` and `retryable`.** `503 vpc_degraded {retryable:true}` tells a client
   to retry; a bare 500 does not.

## NodeDO

One Durable Object per node, addressed by `node_id`. It holds `conn_id`, accepted sockets, open
streams — log tail, exec, port forward, file transfer, each with a ring buffer — and leases.

It is not the job ledger. Job rows live in D1 and are written on transition only — submitted,
started, finished. The split is billing: Durable Object duration bills a flat memory allocation,
and Durable Object SQLite bills row writes per million rows. A log line per row is a metered
write, and a build emits far more log lines than state transitions, so log bytes go to R2.

The DO hibernates between requests. WebSockets are accepted only while a dashboard streams logs;
the agent connection is what wakes it. `onConnect` fences the predecessor by id.

## Connectivity

One `vpc_networks` binding per site. It reaches services on a node that the agent does not
front — a celld peer listener, an S3 gateway — with no public hostname on any node:

```jsonc
"vpc_networks": [{ "binding": "SITE", "tunnel_id": "<uuid>", "remote": true }]
```

The agent's own control path is not on this binding. The agent dials out and holds the
connection (see [`02-agent.md`](02-agent.md)), and its local API is a unix socket that no route
reaches.

- The tunnel MUST run QUIC with outbound UDP 7844 open. An `http2` tunnel breaks VPC resolution
  silently while still reporting healthy.
- Workers VPC is public beta and free while beta (2026-10-29). The fallback — public hostname
  plus Access service token — stays behind a flag, because both can change.

Every VPC call goes through one allowlist function with a static host and port table. A route
that builds the destination from request input is an SSRF primitive into the site network.

```ts
const ALLOWED = {
  celld: { host: "10.96.0.5", ports: [8443] },
  s3:    { host: "10.96.0.7", ports: [8333] },
};
function vpcFetch(t: keyof typeof ALLOWED, path: string) { /* host and port pinned */ }
```

A lint rule enforces that `env.SITE.fetch` appears exactly once, inside `vpcFetch`.

## Auth

| Identity | Mechanism | Scope |
|---|---|---|
| Operator | Cloudflare Access JWT | dashboard |
| CI | API token, hashed in D1 | submit jobs, read own jobs |
| Agent | node credential issued at enrolment | its own node only |

Access fronts every hostname: one application per hostname and role, each with a distinct AUD,
so a token for one application does not authorise another. Service tokens are static
credentials with a configured lifetime — one year by default — rotated by hand, with a grace
window in which the previous secret still validates.

The site's Cloudflare API token is dedicated, with tunnel and DNS permissions split across
separate tokens, and an audit-log webhook on every mutation. A compromised Worker means
control-plane takeover of that site; the data plane cannot be keyed from it.

The Worker holds the control-plane credentials: the Cloudflare API token and the enrolment
authority. Each agent holds its node credential and storage credential. The browser holds none.

## Degradation

| Failure | Dashboard | Fleet |
|---|---|---|
| Agent disconnected | last-known state, actions disabled | workloads keep running under the agent |
| VPC degraded | banner; actions return `503 retryable` | unaffected; the connection returns with the path |
| D1 unavailable | read-only from cache | running workloads unaffected; no new placement or spec change |
| Cloudflare outage | unreachable | **fleet keeps running** |

The data plane runs on the nodes; a Cloudflare outage removes visibility, not execution.
