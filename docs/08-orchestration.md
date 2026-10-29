# 08 — Cell orchestration

A cell is a single-threaded actor with one owner at a time. This is the coordination model for
the Workers/DO layer on a self-hosted site: how a node claims a cell, how ownership survives a
partition, and how a version rollout keeps two owners off the same cell. The agent's own
lifecycle is in [`02-agent.md`](02-agent.md); the control-plane Worker and its `NodeDO` are in
[`05-control-plane.md`](05-control-plane.md).

## The substrate

A site runs N identical celld nodes and one object store bucket. There is no database and no
message bus. Ownership of a cell is a compare-and-swap on a small record in the bucket:

- `If-None-Match: *` claims an unowned record;
- `If-Match: <etag>` renews or hands over a record the caller still holds.

The node that wins the write owns the cell until it releases the record or the lease expires.
Every other node learns ownership by reading the record, or from a routing push (mechanism 3).

The model covers 2–10 hosts per site. At that size there is no consensus group to form: the
bucket is the single serialisation point and the store evaluates each conditional write
atomically. A second owner is possible only through a store that does not implement `If-Match`,
which is why the conditional-write requirement in [`04-storage.md`](04-storage.md) is a
placement constraint.

```
  node-1 ─┐
  node-2 ─┼── CAS ──▶ celld-<app> bucket
  node-3 ─┘            cells/<id> → {owner, epoch, lease_until, version}
```

| Field | Meaning |
|---|---|
| `owner` | `node_id` of the current holder |
| `epoch` | monotonic counter, bumped on every takeover |
| `lease_until` | expiry the holder renews before it lapses |
| `version` | build the cell was started with, used by drain (mechanism 6) |

The epoch is per cell, not per node: two cells owned by the same node carry independent
counters, and a takeover of one does not fence writes to the other.

### Ownership transitions

| Transition | Conditional write | Epoch | Owner after |
|---|---|---|---|
| Claim | `If-None-Match: *` | set to 1 | caller |
| Renew | `If-Match: <etag>` | unchanged | caller |
| Release | `If-Match: <etag>`, owner cleared | unchanged | none |
| Takeover | `If-Match: <etag>` after `lease_until` | bumped by one | caller |

Renew and takeover have the same write shape; `lease_until` decides which one the store is
being asked for. Only a takeover bumps the epoch, so a routine renewal does not invalidate the
commands already in flight on a warm cell.

`lost_after` is the interval a cell stays `unknown` before the control plane re-places it. It
defaults to `lost_threshold_ms` (40000 ms), so the period in which a partition looks like death
is the same on both sides.

## The ten mechanisms

A plain lease is not sufficient on its own. Each mechanism below closes a failure the lease
alone leaves open.

| # | Mechanism | Problem it closes | Normative rule |
|---|---|---|---|
| 1 | Epoch fencing (Rivet actor generations, Golem oplog epochs) | The dual-write window during takeover: the old holder and the new holder both believe they own the cell and both write | Every cell command and every bucket write carries a monotonically increasing cell epoch. A takeover bumps the epoch. A write carrying a superseded epoch fails the conditional write atomically, so the loser learns at its next write rather than at its next read |
| 2 | Advertised lost threshold (Rivet `envoy_lost_threshold`) | The two sides disagreeing on when a cell is lost, so the agent re-places while the control plane still counts the node live, or the reverse | The control plane advertises `lost_threshold_ms` (default 40000) at connect. It MUST exceed the agent's `reconnect_max_ms` (30000), so the agent exhausts its reconnect budget before the control plane judges the node lost |
| 3 | Routing-table cache with rev-tagged pushes (Dapr placement table, Golem lease renewals) | A bucket read on every request, to find which node owns a cell | Nodes cache the cell→node table. Each push carries a `rev`; a push whose `rev` is not newer than the cached one is ignored, and each entry carries its cell's epoch. The bucket is touched on ownership change only, not per request |
| 4 | Sticky affinity with timeout fallback (Temporal sticky execution, Hatchet sticky assignment) | Cold-start cost on every follow-up to a warm cell, and a dead sticky target holding the traffic | Follow-ups route to the node that last held the warm cell. If the cell has not started within the sticky timeout (default 5 s), the request falls back to the routing table |
| 5 | Reminders versus timers (Dapr reminders, Rivet alarms) | In-memory timers dying with the cell, so scheduled work is lost on hibernation | In-memory timers fire only while a cell is warm. Durable reminders are persisted in the bucket and re-activate a hibernated cell. Alarms persist across sleep and re-arm on wake |
| 6 | Drain on upgrade with rate-limited eviction (Rivet `drain_on_version_upgrade`, Temporal Worker Versioning, DBOS versioning) | A rollout in which two code versions own cells of the same name, and an eviction storm when the old build is withdrawn | New builds take new cells. Old cells finish pinned to the build that started them, then drain. Eviction is rate-limited by three knobs: a client head-start delay, a batched period, and a per-cell rate. Rollback is deploying the old code at a higher version, not lowering a version |
| 7 | Disconnected-unknown with a reconcile policy (Nomad `disconnect` block) | Treating a partition as death, and re-placing a cell that is still running | A missed watchdog marks a cell `unknown`, not dead, for `lost_after`. On the node's return a policy resolves ownership: `keep_original`, `keep_replacement`, or `longest_running` |
| 8 | Idempotent enqueue (Inngest) | A replayed command executing twice after a dropped reply | Commands carry a client event ID. The broker deduplicates replays by that ID before dispatch, so a retry is safe at any point in the path |
| 9 | keepAwake and waitUntil (Rivet lifecycle) | Sleep killing work that is mid-flight, and one grace budget spent by the wrong scope | Two sleep-suppression scopes: `keepAwake` blocks idle sleep entirely; `waitUntil` blocks only the grace finalise. Sleep grace is one budget covering `onSleep`, `waitUntil` and async WebSocket handlers |
| 10 | WebSocket hibernation across migration (Rivet, beta) | Client sockets dropped when a cell moves between nodes | A client WebSocket survives a cell move. Target capability, not a v1 requirement |

## What is not built

| Component | Why it is absent |
|---|---|
| Raft or quorum group per site | The bucket is the single serialisation point and evaluates each conditional write atomically; a quorum would serialise the same decisions twice |
| etcd shard-manager mode | Placement is one compare-and-swap on a record, not a watched lease registry; the routing table carries the same information |
| Temporal-style history and matching cluster | Cells are keyed singletons with alarms; multi-step workflow history is a separate workload and its service, database and index are not on the cell path |
| Plan queue or evaluation broker | The control plane places cells directly; no scheduler evaluates constraints in a loop |
| Two-tier fair queue | Command volume is bounded by the cell count per site, and enqueue is already deduplicated (mechanism 8) |
| Gossip routing mesh | The routing table is pushed from one place and is small enough to hold in memory |
| Tablet or depot storage engine | State is plain objects in the bucket plus per-cell SQLite; there is no LSM to compact or tablet to split |

## A worked sequence

```
# 1. claim — node-2 takes an unowned cell
PUT  celld-app/cells/c-17
If-None-Match: *
     {"owner":"n-2","epoch":1,"lease_until":1793000060,"version":"b41"}
→ 200  etag e1

# 2. a command carries the epoch and an event id
→ cmd {cell:"c-17", epoch:1, event_id:"ev-91", op:"incr"}
PUT  celld-app/cells/c-17/state
If-Match: e1
     {"epoch":1,"value":18}
→ 200  etag e2

# 3. node-2 stalls past lease_until; node-5 wins the record
PUT  celld-app/cells/c-17
If-Match: e2
     {"owner":"n-5","epoch":2,"lease_until":1793000120,"version":"b41"}
→ 200  etag e3

# 4. node-2 returns and writes at the superseded epoch
PUT  celld-app/cells/c-17/state
If-Match: e2
     {"epoch":1,"value":19}
→ 412 Precondition Failed   # e2 no longer current, epoch 1 < 2
   node-2 drops the cell and re-reads the record: owner n-5, epoch 2

# 5. a second cell, c-21, on node-2: the watchdog gap exceeds lost_threshold_ms (40000)
   control plane marks c-21 unknown, not dead; no re-placement yet

# 6. the gap passes lost_after: the control plane re-places c-21 on n-5
PUT  celld-app/cells/c-21
If-Match: e7                 # the record n-5 last read
     {"owner":"n-5","epoch":2,"lease_until":1793000300,"version":"b41"}
→ 200  etag e8

# 7. node-2 returns after lost_after, still running its copy of c-21
   STATE reports c-21 on n-2; the control plane applies the reconcile policy:
#      keep_original    → n-2 keeps the cell, at a new epoch
#      keep_replacement → n-5 keeps the cell, n-2 stops its copy
#      longest_running  → the instance with the earlier start time keeps it
```

Step 4 is the case the epoch exists for: `e2` was a valid etag when node-2 read it, so the
conditional write alone would have accepted the record. The epoch makes the staleness visible
to the same atomic operation that checks the etag.

Steps 5–7 separate a partition from a death. The cell is `unknown` for `lost_after` before it
is re-placed, and the policy decides which copy survives when both nodes are alive and both
have a running instance.
