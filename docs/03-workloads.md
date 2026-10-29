# 03 — Workloads

A payload runs at one of three isolation levels, chosen by the provenance of the code.

| Level | Runtime | Isolation mechanism | Start | Use |
|---|---|---|---|---|
| Worker | `workerd` child process | V8 isolate, no ambient syscall surface | ~5 ms | application source written in-house |
| Container | Docker Engine API | namespaces and cgroups on the node kernel | ~200 ms | in-house services, CI over in-house code |
| microVM | Firecracker | separate guest kernel under the jailer | ~125 ms | code the platform did not write |

**Rule: anything not written in-house runs in a microVM.** A CI job that runs `npm install`
executes stranger code, and a container shares the node kernel: a kernel defect is an escape.

---

## Workers

`workerd` runs as a supervised child process of `cfbd`, one process per application. Restarts
follow the payload restart backoff: 100 ms doubling to a 60 s cap, reset after 10 s healthy.

```
cfbd ──spawn──▶ workerd serve /var/lib/cfbd/apps/<app>/config.capnp
```

`workerd` is the runtime Cloudflare runs for Workers, Apache-2.0, with the same semantics as
production: isolate boundaries, bindings, `waitUntil`, alarms.

An application is deployed as a bundle plus a generated `config.capnp`. The control plane
builds both from the application's source, stores them under the application's prefix, and
sends the agent a manifest carrying the digest of each. The agent fetches both, verifies each
digest against the manifest, and refuses a mismatch; the config is the more dangerous artifact,
because a `disk` or `socket` service in it reaches host paths and host sockets as the agent's
user.

The agent creates the new directory under `/var/lib/cfbd/apps/<app>/` with mode `0750`, owned by
root and not writable by the workload's uid, writes the pair into it, and replaces the active
symlink with `renameat2(RENAME_EXCHANGE)` from a random name. The previous directory is removed
after the restart succeeds. The restart gap is ~5 ms.

A ~5 ms gap is invisible to a request/response application. A long-lived socket needs
hibernation and reconnect; see [`08-orchestration.md`](08-orchestration.md).

### Durable Objects

`workerd` alone provides single-node Durable Object storage: an object is durable on the node
that hosts it and nowhere else. Two options cover the remaining cases.

| Option | Provides | Constraint |
|---|---|---|
| celld | Durable Objects, KV, Queues, D1 and Workflows from an unmodified `wrangler.json`, coordinated across a fleet | one application per fleet |
| `workerd` with its own DO storage | one application on one node | single-node durability; node loss loses the object |

celld v0.6.3 (2026-10-21), Apache-2.0, is a single Rust binary that coordinates a fleet
through a shared bucket. It requires an object store with conditional writes, exact ranged
reads and read-after-write consistency.

| Object store | Backs a celld fleet |
|---|---|
| R2, S3, GCS, Azure, Tigris | yes — the stores celld lists as compatible |
| SeaweedFS | conditional writes via the owner filer; verify before use |
| Garage, B2, Hetzner, DO Spaces | no — no conditional writes |
| RustFS | implements `If-Match`; exact ranged reads unverified |

Garage implements no conditional writes, so it cannot back a celld fleet. B2, Hetzner and DO
Spaces implement none either. RustFS implements conditional writes but is not in celld's
compatibility list, so the property to verify is exact ranged reads.

celld runs one application per fleet. Several applications mean one fleet and one bucket per
application, each with its own coordination traffic and RPO target (see
[`04-storage.md`](04-storage.md)). Where no Durable Objects are needed, celld is not deployed.

---

## Containers

The agent speaks one client: the Docker Engine API, HTTP/JSON over a unix socket. Probe order
at startup, first hit wins:

1. `/var/run/docker.sock`;
2. `$XDG_RUNTIME_DIR/podman/podman.sock`, then `/run/podman/podman.sock`;
3. systemd transient units via `StartTransientUnit`;
4. `fork/exec` plus cgroup v2 files.

Podman's compat service serves the same HTTP paths, so Podman is a socket path, not a code
path. The client is a unix stream plus `serde_json`: no gRPC, no protobuf, no codegen.

### Runtime surfaces

| Runtime surface | Provided free | Missing |
|---|---|---|
| Docker / Podman API | create, start, logs, exec, `RestartPolicy`, `Memory`, `NanoCpus`, `PidsLimit`, image pull | nothing on the workload path |
| containerd CRI | image pull, exec, resource update, stats | restart policy, log API, one-call "run this image" |
| systemd transient unit | cgroup limits, `Restart=`, journald | image pull, namespace isolation, exec |
| `fork/exec` + cgroup v2 | nothing | everything |

Exec is the discriminator: Docker, Podman and containerd expose it, systemd does not, and
Firecracker needs a guest agent; containerd's CRI surface is the largest of the four, has no
restart-policy field, and leaves restarts to a kubelet that is not part of this design.

The runtime supplies image pull, cgroup limits, restart, log capture and exec. The agent
supplies reconciliation, port allocation, artifact upload and the job lifecycle.

### The CI job

```
spec = {
  image:   "registry/app/ci@sha256:<64 hex>",
  cpu:     4000,                          # millicores
  memory:  8192,                          # MiB
  timeout: 2700,                          # seconds
  mounts:  [{ from: "cache/<project>", to: "/cache" }],
  env:     { ... },
}
```

`image` is a digest, never a tag: a tag can be repointed after review, and a digest cannot. The
agent re-hashes what it pulls and refuses a mismatch.

The spec is the same at either isolation level. A job whose dependencies are not written
in-house — anything that runs `npm install`, a compiler plugin, or a downloaded toolchain — is
placed in a microVM by the rule above, and the container level is for jobs over in-house code
only.

The agent starts the workload, streams logs, and on exit uploads `/workspace/artifacts` with
a presigned URL generated locally. The control plane records the object key, never the bytes.

---

## microVMs

Firecracker is the microVM level, one microVM per job, for code the platform did not write.
Lifecycle:

1. Resolve the image to a cached ext4 rootfs, content-addressed by digest.
2. Allocate a slot: a tap device, a `/30` from a per-node `/16`, and a per-slot nftables
   ruleset.
3. Boot the VM with the jailer: seccomp filter, cgroup limits, chroot, dedicated uid/gid.
4. Stream the console to the job log.
5. On exit, tear down the slot and garbage-collect the rootfs.

The VM's only route is a node-local pull-through proxy on one port. The proxy holds the
storage credential and enforces the VM's prefix, so a job reads its own inputs, writes its own
artifacts, and reaches nothing else. Firecracker filters no traffic itself: the nftables
ruleset written per slot is the boundary, and a per-node rule is not a substitute.

### Sandbox runtimes

| Runtime | Host requirement | Use |
|---|---|---|
| Firecracker direct | KVM | the level above, under the agent's own slot allocator |
| microsandbox | KVM, WHP or Apple Silicon | the same level through a packaged API, where its lifecycle fits |
| gVisor | no KVM required | hosts without KVM |
| E2B Embed | single node | single-node sandbox service |

Untrusted code requires KVM-grade isolation: a separate kernel boundary rather than a
shared-kernel namespace sandbox. A host that cannot provide one runs no untrusted payload.

---

## Cloudflare Containers

Cloudflare Containers are a candidate host for the same container workloads. The properties
that decide the fit:

| Property | Cloudflare Containers | An owned node |
|---|---|---|
| Instance ceiling | 4 vCPU / 12 GiB / 20 GB, raisable on request | the node's own capacity |
| Image storage | 50 GB per account | the node's disk |
| Host interruption | "does not guarantee that any container instance will run for a set period" | the node runs until it is stopped |
| Disk | ephemeral, fresh from the image on every start | volume mounts |

There is no fixed maximum runtime. The default class sleeps after 10 minutes of inactivity,
and `setInactivityTimeout()` reaches 6 hours; a long build is possible with an alarm. The
interruption model and the per-account image budget keep CI on owned nodes.

---

## Scheduling

The control plane places a workload by filtering and sorting. There is no scheduler process
and no second control plane.

```
candidates = nodes with the required capability and free capacity
chosen     = the candidate with the most free memory
```

Capability comes from the agent's declared `capabilities` list and the host probe behind it;
free capacity comes from the 3 s metrics push. A placement is one query against D1, and the
filter-and-sort holds at fleets below a few hundred nodes, where the build dominates latency.

A scheduler becomes necessary under gang scheduling (every member of a job placed together),
heterogeneous GPU classes (a placement must match an accelerator model) and preemption
(higher-priority work evicts lower). None of the three is present at the current scale.
