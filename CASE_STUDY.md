# Cinder Gateproof Case Study

This branch (`case-study-end`) and `case-study-start` preserve the raw, unedited history of a ~48 hour Gateproof case study: building Cinder entirely through the plan-first method with minimal human intervention.

**Timeframe**: March 2026

**Tags**:
- `cinder-case-study-start-2026-03` – beginning (codex/cinder-step-0)
- `cinder-case-study-end-2026-03` – end state (codex/cinder-pure-loop)

**How to reproduce the proof**: Run `bun run plan.ts` after provisioning infra with `bun run alchemy.run.ts`. Requires `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN`, `GITHUB_TOKEN`, and fixture repo config.

**Preservation**: Protect these branches (no force-push, no deletion) via GitHub Settings > Branches > Add rule for `case-study-start` and `case-study-end`.
