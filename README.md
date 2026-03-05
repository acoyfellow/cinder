# cinder

open source CI runner acceleration. self-hosted. all cloudflare.

```yaml
# the entire install
runs-on: [self-hosted, cinder]
```

cinder makes your github actions builds faster by caching deps at the cloudflare edge. no vendor lock-in. no egress fees. your compute, your cache, your infra.

---

## how fast

the gateproof case study proves a real cold run and a real warm run on the canonical Cargo fixture repo. `speed-claim` only passes when the warm run is measurably faster than the cold run (`SPEED_THRESHOLD_MS`, default `60000`).

cache lives in R2 and is served through a dedicated cache worker. runners restore before execution and upload after execution.

**Demo:** Run the full proof locally (webhook, queue, runner, cache, speed claim). See [examples/](examples/).

---

## how it works

```
github webhook
  → cinder orchestrator   (cloudflare worker)
    → job queued           (durable object)
      → your agent picks it up
        → cache pulled from R2
          → CARGO_HOME + CARGO_TARGET_DIR staged for the run
            → cargo build runs
              → cache archive pushed back to R2
```

current proven cache key: `sha256(Cargo.lock)` from the fixture repo.

---

## quickstart

**1. deploy cinder to your cloudflare account**

```bash
bun add -g @acoyfellow/cinder
cinder deploy
```

takes about 30 seconds. provisions a worker, two durable objects, an R2 bucket, and a KV namespace.

**2. start an agent on your own compute**

```bash
cinder agent start \
  --url $CINDER_URL \
  --token $CINDER_TOKEN
```

runs on hetzner, fly.io, your laptop, anywhere. the agent polls for jobs and manages the local cache staging dir.

**3. update your workflow**

```yaml
jobs:
  build:
    runs-on: [self-hosted, cinder]
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release
```

that's it. no other changes.

---

## supported ecosystems

currently proven in this repository:

| lockfile | cache dirs staged by agent |
|---|---|
| `Cargo.lock` | `CARGO_HOME`, `CARGO_TARGET_DIR` |

other ecosystems are roadmap work, not part of the current proven implementation.

---

## requirements

- cloudflare account (free tier is fine)
- `bun` for deployment
- one machine to run the agent (any linux box)

---

## how-to

**add a second agent**

```bash
cinder agent start --url $CINDER_URL --token $CINDER_TOKEN --labels self-hosted,cinder,arm64
```

agents are stateless. run as many as you want. the orchestrator routes jobs by label match.

**rotate the auth token**

```bash
cinder token rotate
```

**self-host the state store in your own R2**

by default cinder manages its own state. to use a bucket you already have:

```bash
cinder deploy --state-bucket my-existing-bucket
```

if you are switching an already-running stage to a new state bucket, use a clean stage for that migration path.

---

## why cloudflare

github's default runners cache to s3 in us-east-1. blacksmith and depot cache to their own datacenters. cinder caches to R2 — which means your cache hits the edge node nearest your agent, not a fixed region. and R2 has no egress fees, so large caches don't surprise you on a bill.

the orchestrator is a worker with two sqlite-backed durable objects. runner state and job queues survive worker eviction without any external database.

---

## reference

**cinder deploy flags**

```
--account-id       cloudflare account ID (or CLOUDFLARE_ACCOUNT_ID)
--api-token        cloudflare API token  (or CLOUDFLARE_API_TOKEN)
--state-bucket     R2 bucket name for alchemy state (optional)
--region           R2 bucket region hint: wnam, enam, weur, eeur, apac (default: auto)
```

**cinder agent flags**

```
--url              cinder orchestrator URL (or CINDER_URL)
--token            auth token             (or CINDER_TOKEN)
--labels           runner labels, comma-separated (default: self-hosted,cinder)
--poll-ms          job poll interval in ms (default: 1000)
--cache-dir        local staging directory (default: /tmp/cinder)
```

**orchestrator endpoints**

| method | path | auth |
|--------|------|------|
| `POST` | `/webhook/github` | webhook secret |
| `GET` | `/jobs/next` | bearer token |
| `POST` | `/runners/register` | bearer token |
| `DELETE` | `/runners/:id` | bearer token |
| `POST` | `/cache/restore/:key` | bearer token |
| `POST` | `/cache/upload` | bearer token |

**resources deployed**

| name | type |
|------|------|
| `cinder-orchestrator` | cloudflare worker |
| `cinder-cache-worker` | cloudflare worker |
| `cinder-cache` | R2 bucket |
| `cinder-runner-state` | KV namespace |
| `RunnerPool` | durable object (sqlite) |
| `JobQueue` | durable object (sqlite) |

---

## proving cinder with gateproof

for the gateproof proof loop, provisioning and proving are separate on purpose:

1. fill in [`.env.example`](./.env.example) or the local ignored [`.env`](./.env)
2. run `bun run provision` once
3. run `bun run prove`

`alchemy.run.ts` writes `.gateproof/runtime.json` after provisioning. `plan.ts` reads that file so the proof loop can rerun without reprovisioning.

This case study starts from the smallest honest state:

- `alchemy.run.ts` can build and provision the world once
- `plan.ts` can fail against the live system
- the first failing gate tells you what the next commit must do

---

## repository layout

`main` is the clean, curated history. The case-study branches (`case-study-start`, `case-study-end`) and tags (`cinder-case-study-start-2026-03`, `cinder-case-study-end-2026-03`) preserve the original ~48h Gateproof build. A read-only archive lives at [acoyfellow/cinder-case-study](https://github.com/acoyfellow/cinder-case-study). See [CASE_STUDY.md](https://github.com/acoyfellow/cinder/blob/case-study-end/CASE_STUDY.md) on `case-study-end` for details.

---

## contributing

rust all the way down. `crates/cinder-agent` for the agent binary, `crates/cinder-orchestrator` for the orchestrator worker, `crates/cinder-cache` for the cache worker, `crates/cinder-cli` for the CLI.

open an issue before writing code. the roadmap is public.

---

*docs at [cinder.coey.dev](https://cinder.coey.dev) · [github.com/acoyfellow/cinder](https://github.com/acoyfellow/cinder) · built on cloudflare*
