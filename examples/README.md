# Cinder demo

This demo runs the full Cinder proof: webhook intake, job queue, runner execution, cache restore, cache push, and the speed claim. All gates must pass for the proof to succeed.

## Prereqs

- Cloudflare account (free tier fine)
- GitHub PAT with repo access
- Webhook secret (generate one for the fixture repo)
- `bun` and `cargo` installed

## Run

1. From the cinder repo root, copy `.env.example` to `.env` and fill in the required values:
   ```bash
   cp .env.example .env
   # Edit .env with CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_API_TOKEN, GITHUB_PAT, etc.
   ```

2. Run the demo:
   ```bash
   bun run demo
   ```
   Options: `--provision-only` (provision infra only), `--prove-only` (skip provision, run plan only).

3. Expect ~10–15 minutes. The script will:
   - Provision infra (if `.gateproof/runtime.json` is missing)
   - Run the plan (harness and agent are started by the plan when needed)
   - Print a summary with status and per-gate results

## Output

At the end you'll see a summary: overall pass/fail and each gate's status (webhook, queue, runner, cache-restore, cache-push, speed-claim). The speed-claim gate passes when the warm run completes with a cache hit and is measurably faster than the cold run.

For production use, see the [root README](../README.md) quickstart.
