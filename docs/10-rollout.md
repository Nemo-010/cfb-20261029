# 10 — Rollout

Migration is six phases. Each is usable on its own: it ships, carries real traffic, and is
measured before the next one begins. Do not parallelise them, because each phase changes what
normal looks like — a node that reports, then a node that runs containers, then a node that
uploads artifacts — and a phase that lands while the previous one is still settling has no
baseline to be compared against. A phase that cannot be rolled back on its own is not ready to
start. The unit of migration is the phase, not the feature: a capability shipped halfway leaves
two ways to do one thing, and the older one keeps running because nothing has proven the newer
one works.

## Phases

| Phase | What moves | Exit criterion | Rollback |
|---|---|---|---|
| **0 — the manager** | `cfb-manager` as one Worker: Hono for `/api/*`, static assets for the SPA, the D1 schema for nodes, workloads, jobs and audit, Cloudflare Access in front of every hostname, and `NodeDO` with connection fencing. A fake agent connects and reports, so the dashboard is exercised before a host exists. | Log in and see one fake node. | Delete the Worker and the D1 database. No host has been touched. |
| **1 — the agent** | `cfbd` and `/etc/cfbd/config.toml`: dial and reconnect with 1 s → 30 s jittered backoff; `msg_id` RPC correlation with `disconnected` on drop; multiplexed streams with credit flow control; the `exec` capability streaming stdout and stderr; `metrics` at 3 s; the local API at `/run/cfbd/agent.sock`; enrolment by token exchange, credential written `0600`. | A real node connects, reports, and `cfbd doctor` answers offline. | Stop and remove the unit. No workload has moved, so the host returns to its previous role. |
| **2 — workloads** | Docker Engine API detection in the probe order of [`03-workloads.md`](03-workloads.md); `container` start/stop/rm with cgroup limits; restart policy with a crash-loop budget; the log ring buffer and shipping; desired-state reconcile keyed on `generation`; filter-and-sort placement. | A CI job runs from the dashboard with a live log and a stored artifact. | Jobs return to the existing CI. Disable `container` on the node; workloads are removed and their logs stay in the ring buffer until it wraps. |
| **3 — storage** | `storage_url` parsing and an S3 client in the agent; presign at the node so an upload never transits the Worker; bucket layout and lifecycle rules from [`04-storage.md`](04-storage.md); artifacts off R2, logs staying on R2. | Artifacts flow without touching Cloudflare. | Point `storage_url` back at the R2 endpoint. Object keys are unchanged, so earlier artifacts stay readable from either store. |
| **4 — portability** | `@cfb/port` with its eleven capability surfaces; the lint rule that confines backend checks to `packages/port/`; dual-build CI against Cloudflare and `workerd`; fork tests for the nine adapter-backed forks; one real application migrated through the contract. | One application builds twice from one source and passes the same test suite on both. | The application keeps its Cloudflare build. The adapter is additive; no application source depends on it until the swap. |
| **5 — Workers runtime and isolation** (optional) | `workerd` as a supervised child process; bundle upload and `config.capnp` generation; swap on deploy; celld for Durable Objects, one fleet per application with a dedicated bucket; the Firecracker rootfs pipeline, slot allocator, jailer, pull-through proxy and rootfs GC. | An application already running on Cloudflare runs on a node at the site, and an untrusted build has no route except one port on its own node. | Return the application's DNS to Cloudflare. The node keeps the previous bundle directory and reverses the symlink swap. MicroVM jobs stop; untrusted builds return to a managed sandbox or stop. |

The connection is the part that cannot be replaced cheaply later. Every later capability is a
call over it, and a protocol change is an agent release on every node at once. Phase 1 therefore
ships the frame codec, `HELLO` negotiation and the stream layer before any capability is added
on top of them.

Every phase is a deploy of `cfb-manager` plus, from phase 1, a release of `cfbd`. The dashboard
carries a flag per phase, and the flag is removed once every node runs the new path. A flag left
in place is two behaviours kept alive, and the one with fewer users is the one that breaks
unnoticed.

## Order and gates

- Phase 1 precedes every capability. `exec`, `container` and `storage.proxy` are
  calls over a connection that must exist first.
- Phase 2 precedes phase 3. A build produces the artifact that phase 3 moves off R2; until
  containers run there is nothing to upload.
- Phase 3 precedes phase 4 in use. The dual build runs one test suite on both targets and needs
  a job to run it in.
- Phase 4 does not depend on phase 5. An application can be portable and still run only on
  Cloudflare.
- Phase 5 is the only optional phase. It starts when a workload needs Workers execution on
  owned hardware, or untrusted isolation on a node.
- No phase withdraws what an earlier one installed. `exec`, the local API and the state report
  stay available on every node at every phase.
- Phase 0 is the only phase that deploys with no node present. From phase 1 onward, a manager
  change is deployed first and observed against the nodes already connected.

## Scope boundaries

v1 excludes the following by decision. Each has a stated substitute, so a workload that needs
one is visible rather than silently degraded.

| Excluded | Position in v1 |
|---|---|
| Gang scheduling | A placement satisfies the properties of one workload. A job whose members must land together runs as one workload, or waits for a scheduler. |
| Preemption | Placement never evicts. A workload without capacity stays `pending` until capacity appears. |
| Affinity beyond labels | `labels` is a filter in the placement query; there is no node affinity, anti-affinity or topology spread. |
| A VM provisioning API | Nodes are enrolled one installer at a time. Nothing in the control plane creates a machine. |
| ACL or mTLS as the primary authorization model | The agent's declared capability set is the policy: the control plane refuses a capability the node has not declared, and the node refuses one local policy removed. mTLS stays optional where a private CA exists, as transport authentication rather than authorization. |
| Scheduler HA | One control plane per site, to 200 nodes. The data plane keeps running while the control plane is unreachable, and there is no second scheduler to fail over to. |
| Hosting other organisations' workloads | One operator, one trust domain, one D1. A tenant's code runs in a microVM and a tenant's data sits under a prefix; the control plane is not shared. |

The capability policy is structural: a control plane that has lost integrity cannot grant a node
a capability the node does not have, and an edge ACL would authorise the caller rather than the
operation. The same policy bounds each node: an agent that has lost integrity holds only the
capabilities it declared, and revoking the node credential ends its access.

Scheduler HA is out because the recovery path does not need it. On reconnect the agent sends a
full state report and the control plane diffs from scratch, so an outage costs visibility and
new placements, not running work. A second control plane would add a second placement decision
over one D1.

## First milestone

Three deliverables carry the design: the agent, the manager and the portability adapter. They
are phases 0, 1 and 4, in that order, and each is complete when its exit criterion is observed,
not when its list of items is closed.

Phases 2 and 3 make a CI workload possible; phase 5 waits until a workload needs Workers
execution or microVM isolation on a node. A fleet at phase 1 with no containers is a working
control plane, a working agent and a real diagnostic path. A fleet at phase 4 without phase 5 is
an application that builds twice from one source, which is the requirement the rest of the
design serves.

A phase that no workload demands stays unbuilt. The cost of an unbuilt phase is a capability a
workload cannot use yet; the cost of a phase built early is a protocol, a schema or a bucket
layout that the first real workload then forces into a change.
