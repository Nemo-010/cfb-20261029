# 04 — Storage

Storage is an S3 endpoint the agent is configured with, not a component the platform ships.
`cfbd` holds one URL and one credential, for the bucket role the node serves — `storage_url` in
`/etc/cfbd/config.toml`, `CFB_STORAGE_URL` in the environment — and every read, write and
presign is a SigV4 request against it. The form is
`s3://<bucket>?endpoint=<url>&region=<name>`. Changing the endpoint changes a configuration
value, not application source.

A node serves one role. An artifacts node writes `artifacts` and reads `images`; a cache node
writes `cache`. A `celld-<app>` bucket is not an agent role: celld holds that credential
itself, because a credential that can write an ownership record can take over the fleet. The
`logs` bucket is written by `cfb-manager` through its R2 binding, so no agent holds a logs
credential. A node that served every role would hold a credential that spans every bucket,
which is the arrangement this split exists to prevent.

```toml
# Cloudflare R2
storage_url = "s3://cfb-artifacts?endpoint=https://<account-id>.r2.cloudflarestorage.com&region=auto"

# self-hosted SeaweedFS S3 gateway
storage_url = "s3://cfb-artifacts?endpoint=http://10.96.0.7:8333&region=us-east-1"
```

| Part | Meaning |
|---|---|
| `s3://<bucket>` | the bucket the agent writes to; one URL per bucket role |
| `endpoint=` | scheme, host and port of the S3 API. HTTPS for a public endpoint, HTTP for an address reachable over `SITE` |
| `region=` | SigV4 signing region. R2 uses `auto`; every other backend takes its configured region |

Addressing is path style, `<endpoint>/<bucket>/<key>`; virtual-host addressing needs a per-bucket DNS name.

## The parity bar

R2's S3 API defines what an endpoint has to do. It is the reference, not AWS S3.

| Implemented | Not implemented |
|---|---|
| SigV4 request signing | bucket versioning |
| multipart upload | object lock |
| presigned URLs (GET, PUT, HEAD, DELETE) | S3 event notifications |
| SSE-C with customer-supplied keys | object tagging |
| conditional writes (`If-None-Match`, `If-Match`) | bucket and object ACLs |

A replacement clears the bar when those five implemented rows work. Full AWS conformance is not
required; a store that adds versioning, lock, notifications, tagging or ACLs adds capability the
platform never calls.

## Backends

| Backend | Licence | Fit | Constraint |
|---|---|---|---|
| R2 | proprietary | default; `logs` always | S3 endpoint supports presign and conditional writes; the Worker binding does not expose presign |
| SeaweedFS | Apache-2.0 | large self-hosted | conditional writes via owner-filer serialisation; S3 authentication is off by default |
| RustFS | Apache-2.0 | MinIO-shaped; 1.0 released 2026-09-16 | implements `If-Match` and `If-None-Match`, answering a failed precondition with `412`; not in celld's qualified list, so verify exact ranged reads before a celld fleet |
| Garage | AGPL-3.0 | homelab | no conditional writes; not usable for celld; upstream at `git.deuxfleurs.fr`, GitHub is a mirror |
| MinIO | AGPL-3.0 | — | repository archived 2026-04-25 |
| B2, Hetzner, DO Spaces | proprietary | cheap bulk storage | no conditional writes; not usable for celld |
| VersityGW | Apache-2.0 | S3 gateway in front of a POSIX filesystem | semantics are those of the backing filesystem; the gateway adds no durability |
| Ceph RGW | LGPL-2.1 with Apache-2.0 parts | colo tier needing AWS-shaped semantics | the only listed store with versioning, object lock, notifications and bucket policies; operational weight is the constraint |
| s3proxy | Apache-2.0 | protocol translation | exposes an existing object store over S3; adds a hop and holds no data of its own |

## Conditional writes

celld coordinates a fleet through a shared bucket. It runs no consensus protocol: a cell is owned
by whichever node wins a compare-and-swap on a small ownership record, and the fleet re-reads that
record to learn the current owner. The store therefore has to evaluate `If-None-Match: *` (claim an
unowned record) and `If-Match: <etag>` (renew or hand over a record the caller still holds)
atomically, and it has to serve exact ranged reads under read-after-write consistency.

| Store | `If-None-Match: *` | `If-Match: <etag>` |
|---|---|---|
| S3 (AWS) | yes | yes |
| R2 | yes | yes |
| SeaweedFS | yes | yes, serialised through the owner filer |
| RustFS | yes | yes — `412` on a failed precondition |
| Garage | no | no |
| B2, Hetzner, DO Spaces | no | no |

A store missing either column can hold `images`, `cache`, `artifacts` and `logs`; it cannot hold a
`celld-<app>` bucket. The requirement applies only when running celld, and every other bucket role
works on a store that implements nothing beyond GET, PUT, multipart and list.

RustFS implements both columns; celld's own compatibility list does not include it, so exact
ranged reads are the property to verify before placing a fleet there. Conditional-write support
in Ceph RGW varies by release; verify it before placing a fleet there.

## Bucket layout

| Bucket | Contents | Lifecycle |
|---|---|---|
| `images` | content-addressed OCI layers and rootfs tarballs | immutable, retained indefinitely, deduplicated by digest |
| `cache` | build caches, one prefix per toolchain | delete 14 days after last read |
| `artifacts` | test output, binaries, reports | delete 90 days after write |
| `celld-<app>` | ownership records and fleet state for one celld application | retained indefinitely |
| `logs` | agent and workload logs | retained on R2 regardless of where the rest lives |

`images` is append-only: an object key is the digest of its content, so a rewrite is a new key and
a reader never observes a partial layer. `logs` stays on R2 in every deployment, including a
self-hosted one, because it has to be readable while the site is down.

The `celld-<app>` bucket controls the fleet. A credential that can write an ownership record can
take over every cell in that fleet, so its credential MUST be scoped to that bucket alone: no
account-level list, no access to `images`, `cache`, `artifacts` or `logs`. One bucket per
application, never shared between applications.

## Uploads never transit the control plane

A Worker has 128 MB of memory and a 100 MB request body limit. A build artifact cannot pass
through `cfb-manager` even once, and a build produces many of them. Uploads are presigned at the
node instead.

```
runner ──"sign a PUT"──────────▶ cfbd ──SigV4 presign──▶ S3 endpoint
runner ──PUT to presigned URL─────────────────────────▶ S3 endpoint
cfbd   ──object key, size, sha256──▶ cfb-manager
```

The agent signs because it holds the storage credential. The control plane sees the object key
and the metadata the agent reports; it sees neither the bytes nor the credential. Downloads follow
the same path in reverse: the runner asks the agent, the agent returns a presigned GET, and the
transfer runs between the runner and the store.

The endpoint must be reachable from the payload host, because the presigned URL names that address.
A store behind the site gateway is the normal case.

## Durability

R2 is replicated across Cloudflare infrastructure, so durability is a property of that replication,
not of any one machine. A self-hosted cluster has whatever redundancy is configured on top of it.

| Cluster | Mechanism | What it survives |
|---|---|---|
| 1 node | none | a single point of failure; loss of the host loses the data |
| 3 nodes | replication, two or three copies | loss of one node |
| 5+ nodes, 2+ racks | erasure coding 10+4 | loss of a node or a rack, with repair |

Erasure coding on three nodes loses data. EC 10+4 produces 14 shards for 10 data shards. Three
nodes hold 14/3 ≈ 4.7 shards each, and the layout rule caps a node at 4 shards, so the 14 shards
have no legal placement; even ignoring the cap, one lost node removes about 4.7 shards against 4
parity shards, more than the code can reconstruct. EC 10+4 needs five nodes across two racks
before every shard has a home that survives a rack failure.

EC repair is manual in the open-source build. A failed volume stays under-replicated until a
repair job is run, and no alert fires while it is not. A three-node replication factor plus an
off-site copy of `images` and `celld-*` is the configuration with a restore path, and the restore
is rehearsed before it is needed.

## Retention

| Bucket | Retention | Reason |
|---|---|---|
| `images` | forever | content-addressed and deduplicated; a digest is never rewritten |
| `cache` | 14 days since last read | the largest and least valuable dataset |
| `artifacts` | 90 days, or per project | a policy choice, not a storage limit |
| `celld-<app>` | forever | it is the fleet's state; deleting it strands every cell |

A lifecycle rule on `cache` is mandatory. Without it the bucket grows with every build and the
storage bill tracks build count rather than dataset size. The rule deletes objects not read for 14
days, keeping a cache entry alive exactly as long as something still reads it.
