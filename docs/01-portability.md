# 01 — The portability contract

R1: application source runs unchanged on Cloudflare Workers and on a self-hosted runtime. Nine
of the thirteen forks are absorbed by one adapter package, three are one-time rewrites, and one
has no self-hosted equivalent. This document enumerates the surface, names the forks, and
states what enforces them.

## Two levels

| Level | What changes | Cadence | Owned by |
|---|---|---|---|
| Application runtime | the platform the application process runs on | once per application | the application author |
| Capability | storage, sql, queue, sandbox, actors, cache, secrets, crypto, workflow, clock, geo behind an interface | per deployment | `@cfb/port` |

Only the runtime level is a runtime swap. Exactly two runtime targets exist.

| Target | Runtime | Durable Objects |
|---|---|---|
| Cloudflare | Workers | Cloudflare Durable Objects |
| Self-hosted | `workerd` | celld |

`workerd` executes the Workers programming model from the same source, as a supervised child
process of `cfbd` (see [`03-workloads.md`](03-workloads.md)). celld supplies Durable Object
coordination for a `workerd` fleet: one application per fleet, through a shared bucket that
implements conditional writes, exact ranged reads and read-after-write consistency. No other
runtime executes Workers code; hosted services supply one capability each, never a runtime.

## Level 1 — the Portable Workers Subset

PWS is the binding surface both targets implement: sixteen entries, each running on both from
one source file, and everything outside them is a fork.

| API | Cloudflare | workerd + celld | Portable |
|---|---|---|---|
| ESM `fetch` handler, `env`, `ctx` | yes | yes | **yes** |
| `vars` | yes | yes | **yes** |
| KV | yes | yes | yes — consistency differs, semantics do not |
| R2 / S3 | yes | yes | yes, unless presigned or public URL |
| D1 | yes | yes | yes, single-statement only |
| Queues (produce + consume) | yes | yes | yes, no pull consumers |
| Durable Objects + SQLite | yes | yes | yes, no `jurisdiction` |
| DO alarms | yes | yes | **yes** |
| WebSockets + hibernation | yes | yes | yes, reconnect on cell move |
| Service bindings | yes | yes | yes, no stub passing |
| Workflows | yes | yes | yes, without `schedules` |
| Cron Triggers | yes | yes | yes, plain expressions |
| HTMLRewriter | yes | yes | **yes** |
| WebAssembly | yes | yes | **yes** |
| Static assets | yes | yes | yes, no `.assetsignore` |
| `ctx.waitUntil` | yes | yes | **yes** |

The caveats are properties of the runtimes, not configuration gaps. A program that depends on
a behaviour a caveat excludes is outside PWS: R2 presigned URLs are fork 7, not PWS storage.

### The 13 forks

| # | Fork | Adapter surface |
|---|---|---|
| 1 | Service-worker syntax | one-time rewrite, no adapter |
| 2 | Cache API (always-miss on celld) | `cache` |
| 3 | `node:crypto` ciphers, DH, RSA-PSS, DSA | `crypto` |
| 4 | Web Crypto P-384/P-521, RSA-PSS, secret JWK export | `crypto` |
| 5 | `request.cf` geo/colo/TLS | `geo` |
| 6 | `performance.now()` as a monotonic clock | `clock` |
| 7 | R2 presigned URLs, public URL, `ssecKey` | `storage` |
| 8 | D1 `dump()`, read replicas, `BEGIN`/`ATTACH` | `sql` |
| 9 | Queues pull consumers, HTTP API | `queue` |
| 10 | Workflow `schedules`, rollback, stream results | `workflow` |
| 11 | Cron descending ranges, `*` inside a list | one-time rewrite, no adapter |
| 12 | DO `jurisdiction`, class rename migrations | one-time rewrite, no adapter |
| 13 | Workers AI, Vectorize, Hyperdrive, Browser Rendering | no self-hosted equivalent |

Forks 1, 11 and 12 are rewritten once and stay rewritten; there is no adapter and no fork
test. Fork 13 has no self-hosted equivalent: a workload that needs it is Cloudflare-only.

## Level 2 — capability adapters

Each capability is one interface with N implementations. Application code calls the interface.

| Capability | Cloudflare | Self-hosted | Hosted third party |
|---|---|---|---|
| `storage` | R2 binding | S3: SeaweedFS, RustFS, Garage | Backblaze B2, Tigris |
| `sql` | D1 | SQLite via celld, or Postgres | Neon, Turso |
| `queue` | Queues | celld Queues | — |
| `sandbox` | Sandbox SDK | Firecracker via `cfbd` | E2B Cloud |
| `actors` | Durable Objects | celld DOs | Rivet Cloud |
| `cache` | Cache API | no-op | — |
| `secrets` | Worker secrets | celld `vars` plus an external store | — |
| `crypto` | Web Crypto | `workerd` Web Crypto plus a `node:crypto` shim | — |
| `workflow` | Workflows | celld Workflows | — |
| `clock` | `performance.now()` | a monotonic clock from the agent | — |
| `geo` | `request.cf` | gateway-forwarded headers, or nothing | — |

The hosted column is capability, not runtime. Two entries need the distinction stated:

- **E2B** runs sandboxed code, not Workers code. It backs the `sandbox` adapter where microVMs
  are required without operating Firecracker; one machine per install, multi-node is commercial.
- **Rivet** provides actors behind its own API; it does not run Workers code. Code written
  against `DurableObject` needs its actor code rewritten for Rivet, so Rivet backs `actors` for
  new code, not as a swap for existing code.

Backend constraints — conditional writes for celld, presign and lifecycle — are in
[`04-storage.md`](04-storage.md); secret handling is in [`06-security.md`](06-security.md).

## What changes on a swap

| Swap | Source change | Config change |
|---|---|---|
| Cloudflare → workerd, PWS-only application | 0 lines | `wrangler.jsonc` bindings |
| Cloudflare → workerd, application uses forks 2–10 | adapter only, written once | bindings |
| R2 → SeaweedFS | 0 lines | `storage_url` |
| Cloudflare Sandbox → E2B Cloud | `sandbox` adapter implementation | API key, endpoint |
| Durable Objects → Rivet | actor code rewritten | SDK, config |
| Any target → any target, feature outside PWS | that feature rewritten | — |

## `@cfb/port`

One package, source at `packages/port/`. It is the only place a backend check exists.

```ts
storage   get(key), put(key, body, opts), delete(key), list(prefix), presign(key, ttl)
sql       query(text, params), batch(stmts), migrate(dir)
cache     get(key), put(key, response, ttl)          // may always miss
queue     send(message, opts), consume(handler)
sandbox   run(code, limits) -> { exitCode, stdout, stderr }
actors    namespace(id) -> object
crypto    hash(algo, bytes), hmac(algo, key, bytes), sign(algo, key, bytes), encrypt(algo, key, bytes)
workflow  start(name, input), status(id), cancel(id)
clock     now(), monotonic()
geo       country(request), colo(request), tls(request)
secrets   get(name)
compat    has(feature) -> boolean                    // the only feature probe
```

`packages/port/compat.ts` is the feature-probe module: the single place a Cloudflare-only
product may be imported, and the single place its availability is decided. `has(feature)`
returns a boolean derived from the active backend; callers branch on that value. Backends are
registered, not branched:

```ts
const backend = env.CFB_BACKEND;        // "cloudflare" | "selfhosted" | "hybrid"
const adapters = registry[backend];
```

`env.CFB_BACKEND` is set by the platform, never by the application; the registry is keyed per
capability, so no application file contains `if (isCloudflare)`.

`hybrid` is a supported configuration: Cloudflare runtime with E2B sandboxes and SeaweedFS
storage, or any other per-capability mixture, held in a configuration file.

## Enforcement

1. **Lint.** One rule bans `caches.default`, `request.cf`, `env.BUCKET`, `presign` and direct
   `new DurableObject` outside `packages/port/`. A second bans Cloudflare-only imports outside
   `packages/port/compat.ts`.
2. **Dual build.** CI builds for Cloudflare and for `workerd` and runs one test suite against
   both. A test that passes on one target and fails on the other is a portability bug.
3. **Fork tests.** The nine adapter-backed forks, 2 through 10, each carry a test named
   `portability/fork-NN`. The test fails when its adapter stops covering the fork.
4. **Runtime assertion.** The registry throws when `env.CFB_BACKEND` is missing or holds an
   unknown value, so a misconfigured deployment fails at start rather than at first use.

## What portability does not buy

| Property | Cloudflare | Self-hosted | Consequence |
|---|---|---|---|
| Performance | edge execution near users | one node where it is installed | latency and throughput are deployment properties, not source properties |
| Consistency | KV eventually consistent | KV strongly consistent on celld | code that depends on either ordering is not portable |
| Secrets handling | Worker secrets | celld has no secret store; values land in `vars`, plaintext in the deployment | secret handling is a deployment concern, not a source concern |
| Networking | TLS terminated at the edge | whatever the site gateway provides | inbound reachability and certificate handling differ |
| Workers AI | available | no equivalent | fork 13; a workload that needs it is Cloudflare-only |

The contract covers source, not behaviour. An application that builds on both targets can still
behave differently on each.
