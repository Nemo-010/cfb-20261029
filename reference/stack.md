# Reference — the stack

One table per layer. `Role` states what the project does in the stack. `Constraint` states the
property that decides whether it can be used. Repository slugs are GitHub `owner/name`; a slug
whose upstream is elsewhere says so in the constraint. Status is `active`, `archived <date>`,
`beta` or `announced merge <date>`.

## Workers runtime

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| workerd | cloudflare/workerd | Apache-2.0 | active | runs a Worker bundle as a supervised child process of `cfbd`; the isolate, bindings, alarms and `waitUntil` semantics are the production ones | Durable Objects are single-node: an object is durable on the node that hosts it and nowhere else |
| celld | denoland/celld | Apache-2.0 | active | v0.6.3 (2026-10-21); Workers, Durable Objects, KV, Queues, D1, R2 bindings, Workflows and cron from an unmodified `wrangler.json`, coordinated across a fleet through a shared bucket | one application per fleet; needs an object store with conditional writes, exact ranged reads and read-after-write consistency; merge into workerd announced 2026-10-09 with no published release date |
| Miniflare | cloudflare/miniflare | MIT | archived 2025-03-13 | Workers and R2 simulator for the local development loop | the standalone repository is archived; the maintained code lives inside cloudflare/workers-sdk |

## Object storage

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| R2 | — | proprietary | active | default backend; the `logs` bucket always | S3 endpoint implements SigV4, multipart, presigned URLs, SSE-C and conditional writes, but not bucket versioning, object lock, notifications, tagging or ACLs; the Worker binding exposes no presign |
| SeaweedFS | seaweedfs/seaweedfs | Apache-2.0 | active | large self-hosted backend behind an S3 gateway | conditional writes come from owner-filer serialisation; S3 authentication is off by default |
| RustFS | rustfs/rustfs | Apache-2.0 | active | MinIO-shaped self-hosted backend; 1.0 released 2026-09-16 | implements `If-Match` and `If-None-Match` (a failed precondition is `412`), but is not in celld's list of qualified stores; verify exact ranged reads before running a celld fleet on it |
| Garage | deuxfleurs-org/garage | AGPL-3.0 | active | homelab backend | no conditional writes, so it cannot back a celld fleet; upstream is `git.deuxfleurs.fr` and GitHub is a mirror |
| MinIO | minio/minio | AGPL-3.0 | archived 2026-04-25 | none | repository archived; no longer a backend |
| VersityGW | versity/versitygw | Apache-2.0 | active | S3 gateway in front of a POSIX filesystem | semantics and durability are those of the backing filesystem; the gateway adds neither |
| Ceph RGW | ceph/ceph | LGPL-2.1 with Apache-2.0 parts | active | colo tier needing AWS-shaped semantics | the only store listed with versioning, object lock, notifications and bucket policies; operational weight is the constraint |
| s3proxy | gaul/s3proxy | Apache-2.0 | active | S3 protocol translation in front of an existing store | holds no data of its own and adds a hop |
| JuiceFS | juicedata/juicefs | Apache-2.0 | active | POSIX filesystem layered over an object store | a filesystem client, not an S3 endpoint a celld fleet coordinates through |
| Backblaze B2 | — | proprietary | active | cheap bulk storage | no conditional writes, so it cannot back a celld fleet |

## Sandboxes and isolation

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| Firecracker | firecracker-microvm/firecracker | Apache-2.0 | active | microVM level for code the platform did not write; boot to init is ~125 ms | a VMM only: lifecycle needs an API daemon and isolation needs the jailer |
| microsandbox | superradcompany/microsandbox | Apache-2.0 | active | single-binary self-hosted sandbox with libkrun microVMs, OCI images, snapshots and an HTTP API | requires KVM, WHP or Apple Silicon |
| gVisor | google/gvisor | Apache-2.0 | active | syscall interception on hosts without `/dev/kvm` | container UX with a user-space kernel; the runtime is `runsc`, not a microVM |
| Kata Containers | kata-containers/kata-containers | Apache-2.0 | active | microVM isolation under a Kubernetes container runtime | assumes a container runtime and a Kubernetes-shaped host |
| Cloud Hypervisor | cloud-hypervisor/cloud-hypervisor | Apache-2.0 / BSD-3-Clause | active | alternative VMM for the microVM level | a VMM only, with the same API-daemon gap as Firecracker |
| smolvm | smol-machines/smolvm | Apache-2.0 | active | libkrun microVMs, branchable and portable, with S3 bucket mounts | a runtime primitive with no control plane of its own |
| E2B runtime | e2b-dev/runtime | Apache-2.0 | active | full sandbox backend: API, orchestrator, edge router, `envd`, template builder | the managed multi-node path is commercial; the open Embed deployment is single-node |
| E2B SDK | e2b-dev/E2B | Apache-2.0 | active | client surface for a sandbox API, including computer use | a client library; carries no runtime |
| sandbox-sdk | cloudflare/sandbox-sdk | Apache-2.0 | active | parity target for the sandbox API: per-session sandbox, exec, streams, git, code interpreter | a Cloudflare-only API surface; the bridge is Workers-only by design |

## Actors and workflows

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| Rivet | rivet-dev/rivet | Apache-2.0 | active | virtual actors with per-actor SQLite, realtime broadcast and cron; self-host is one control-plane binary plus a bucket | its own actor API, so Workers Durable Object code must be rewritten; no workerd and no Workers API |
| Temporal | temporalio/temporal | MIT | active | durable multi-step workflows | no keyed singleton actor; service, database and Elasticsearch to operate |
| Restate | restatedev/restate | BSL-1.1 | active | virtual objects and durable handlers in one binary | BSL-1.1; no WebSocket hibernation and no Workers runtime compatibility |
| Golem | golemcloud/golem | BSL-1.1 | active | durable WASM agents keyed by an ID | BSL-1.1; Kubernetes is the recommended deployment |
| Dapr | dapr/dapr | Apache-2.0 | active | distributed runtime with an actor model and sidecar | looser actor semantics than Durable Objects |
| Orleans | dotnet/orleans | MIT | active | virtual actor framework | .NET only |
| PartyKit | partykit/partykit | MIT | active | realtime rooms over Durable Objects | self-hosts onto Cloudflare Durable Objects |

## Connectivity

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| cloudflared | cloudflare/cloudflared | Apache-2.0 | active | outbound-only edge path: public hostnames, DNS and Access policies, no inbound port | self-updates and restarts daily by default |
| Headscale | juanfont/headscale | BSD-3-Clause | active | self-hosted Tailscale control server for the peer overlay | depends on Tailscale's DERP fleet by default |
| NetBird | netbirdio/netbird | BSD-3-Clause, except `management/`, `signal/`, `relay/`, `combined/` = AGPL-3.0 | active | overlay network alternative | the four control directories are AGPL-3.0 while the rest of the tree is BSD-3-Clause |
| Authelia | authelia/authelia | Apache-2.0 | active | origin-side identity and access portal | needs its own user store and session backend |
| Pangolin | fosrl/pangolin | AGPL-3.0 + Fossorial commercial terms | active | self-hosted tunnel, identity and dashboard as one unit | AGPL-3.0 with Fossorial commercial terms |

## CI

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| Woodpecker | woodpecker-ci/woodpecker | Apache-2.0 | active | CI server and container-executing agents | agents poll outbound, so no inbound port; the pipeline YAML is its own dialect |
| Dagger | dagger/dagger | Apache-2.0 | active | build and cache engine under Woodpecker | provides the build engine, not the scheduler |
| Gitea | go-gitea/gitea | MIT | active | git forge with an Actions runner | the Actions runner is a separate hard fork and some marketplace actions do not run |

## Agent prior art

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| cfrs | talaria0101/cfrs | 0BSD | active | socket interposition | 0BSD; every `AF_INET` address is rewritten to the `AF_UNIX` abstract name `\0cfrsnet/<family>/<ip>/<port>`, so an unmodified binary binds real addresses with no root, no network namespace and no `CAP_NET_ADMIN`; its `tunnel` subcommand has no serving loop |
| ServerBee | ZingerLittleBee/ServerBee | AGPL-3.0 | active | `msg_id`-correlated RPC and a capability bitmask | AGPL-3.0, so no code may be taken from it; one agent-dialled WebSocket carries request/reply, waiters are dropped on disconnect, and the bitmask is owned by the agent and mirrored read-only by the server |
| mirrord | metalbear-co/mirrord | MIT | active | fair-queued multiplexer | MIT; a per-queue fair-queued multiplexer behind versioned protocol crates; its `LD_PRELOAD` and iptables interception is Kubernetes-coupled |
| amika | gofixpoint/amika | Apache-2.0 | active | tunnel codec | Apache-2.0; a tunnel multiplexer codec and service-token signing, both Worker-safe; its control plane, UI and relay are closed source |

## Platforms not used

| Project | Repository | Licence | Status | Role | Constraint |
|---|---|---|---|---|---|
| Nomad | hashicorp/nomad | BUSL-1.1 | active | batch scheduling and parameterised dispatch | BUSL-1.1, and quotas are a no-op in the community build |
| Incus | lxc/incus | Apache-2.0 | active | one REST API for VMs and system containers | a second API and auth model for a job the container runtime already does |
| coder | coder/coder | AGPL-3.0 | active | outbound-only workspace agent | AGPL-3.0, with external provisioners and prebuilds behind a paid tier |
| Daytona | daytonaio/daytona | none | archived 2026-07-24 | none | archived with no licence; the successor is closed source |
| OpenFaaS | openfaas/faas | Apache-2.0 | active | container FaaS | a container FaaS and not a Workers model |

All licences and statuses checked 2026-10-29 against the GitHub API and each project's in-tree
licence file. Re-check before relying on a status.
