import { Effect } from "effect";
import crypto from "node:crypto";
import { existsSync, readFileSync, rmSync } from "node:fs";
import type { ScopeFile } from "gateproof";
import {
  Act,
  Assert,
  Gate,
  Plan,
  Require,
} from "gateproof";
import { Cloudflare } from "gateproof/cloudflare";

type RuntimeState = {
  orchestratorName?: string;
  orchestratorUrl?: string;
  fixtureRepo?: string;
  fixtureBranch?: string;
  fixtureWorkflow?: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function readOptionalEnv(name: string): string | undefined {
  const value = process.env[name];
  if (typeof value !== "string") {
    return undefined;
  }

  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function loadRuntimeState(): RuntimeState | null {
  const runtimeFile = new URL("./.gateproof/runtime.json", import.meta.url);

  if (!existsSync(runtimeFile)) {
    return null;
  }

  try {
    const parsed: unknown = JSON.parse(readFileSync(runtimeFile, "utf8"));
    if (!isRecord(parsed)) {
      return null;
    }

    return {
      orchestratorName:
        typeof parsed.orchestratorName === "string" ? parsed.orchestratorName : undefined,
      orchestratorUrl:
        typeof parsed.orchestratorUrl === "string" ? parsed.orchestratorUrl : undefined,
      fixtureRepo: typeof parsed.fixtureRepo === "string" ? parsed.fixtureRepo : undefined,
      fixtureBranch:
        typeof parsed.fixtureBranch === "string" ? parsed.fixtureBranch : undefined,
      fixtureWorkflow:
        typeof parsed.fixtureWorkflow === "string" ? parsed.fixtureWorkflow : undefined,
    };
  } catch {
    return null;
  }
}

function resolveLocalRunnerId(): string {
  try {
    const hostname = readFileSync("/etc/hostname", "utf8").trim();
    return `cinder-${hostname || "unknown"}`;
  } catch {
    return "cinder-unknown";
  }
}

const runtimeState = loadRuntimeState();
const baseUrl = readOptionalEnv("CINDER_BASE_URL") ?? runtimeState?.orchestratorUrl ?? "";
const workerName =
  readOptionalEnv("CINDER_WORKER_NAME") ?? runtimeState?.orchestratorName ?? "cinder-orchestrator";
const fixtureRepo =
  readOptionalEnv("CINDER_FIXTURE_REPO") ?? runtimeState?.fixtureRepo ?? "acoyfellow/cinder-prd-test";
const fixtureBranch = readOptionalEnv("CINDER_FIXTURE_BRANCH") ?? runtimeState?.fixtureBranch ?? "";
const fixtureWorkflow =
  readOptionalEnv("CINDER_FIXTURE_WORKFLOW") ?? runtimeState?.fixtureWorkflow ?? "";
const internalToken = readOptionalEnv("CINDER_INTERNAL_TOKEN") ?? "";

const missKey = crypto.randomBytes(32).toString("hex");
const newKey = crypto.randomBytes(32).toString("hex");
const speedThresholdMs = Number(process.env.SPEED_THRESHOLD_MS ?? "60000");
const testRepo = process.env.TEST_REPO ?? "";
const harnessBaseUrl = "http://127.0.0.1:9000";
const harnessRunUrl = `${harnessBaseUrl}/test/run`;
const localRunnerId = resolveLocalRunnerId();
const agentLogPath = "/tmp/cinder-agent-proof.log";
const agentPidPath = "/tmp/cinder-agent-proof.pid";
const runnerJobPath = "/tmp/cinder-proof-runner-job.json";
const queuePayloadPath = "/tmp/cinder-proof-queue-payload.json";

let managedHarness: ReturnType<typeof Bun.spawn> | null = null;

async function canReachLocalHarness(): Promise<boolean> {
  try {
    const response = await fetch(harnessBaseUrl);
    return response.ok || response.status === 404;
  } catch {
    return false;
  }
}

async function ensureLocalHarness(): Promise<void> {
  if (await canReachLocalHarness()) {
    return;
  }

  managedHarness = Bun.spawn({
    cmd: ["bun", "harness.ts"],
    cwd: process.cwd(),
    stdout: "inherit",
    stderr: "inherit",
  });

  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (await canReachLocalHarness()) {
      return;
    }

    await Bun.sleep(100);
  }

  throw new Error("cinder proof harness did not start on 127.0.0.1:9000");
}

function stopManagedHarness(): void {
  if (!managedHarness) {
    return;
  }

  managedHarness.kill();
  managedHarness = null;
}

function stopManagedAgent(): void {
  if (!existsSync(agentPidPath)) {
    return;
  }

  try {
    const pid = Number.parseInt(readFileSync(agentPidPath, "utf8").trim(), 10);
    if (Number.isFinite(pid)) {
      process.kill(pid);
    }
  } catch {
    // Ignore stale proof agent state.
  }

  try {
    rmSync(agentPidPath, { force: true });
  } catch {
    // Ignore pidfile cleanup failures during shutdown.
  }
}

async function ensureColdBuildBaseline(): Promise<void> {
  if (readOptionalEnv("COLD_BUILD_MS")) {
    return;
  }

  if (!testRepo) {
    return;
  }

  try {
    const response = await fetch(harnessRunUrl, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        repo: testRepo,
        with_cache: false,
      }),
    });

    if (!response.ok) {
      return;
    }

    const parsed: unknown = await response.json();
    if (!isRecord(parsed)) {
      return;
    }

    const buildDurationMs = parsed.build_duration_ms;
    if (typeof buildDurationMs !== "number" || !Number.isFinite(buildDurationMs)) {
      return;
    }

    process.env.COLD_BUILD_MS = String(buildDurationMs);
  } catch {
    // Let the existing prerequisite fail clearly if the harness is unavailable.
  }
}

await ensureColdBuildBaseline();

const workerLogs = Cloudflare.observe({
  accountId: readOptionalEnv("CLOUDFLARE_ACCOUNT_ID") ?? "",
  apiToken: readOptionalEnv("CLOUDFLARE_API_TOKEN") ?? "",
  workerName,
  sinceMs: 120_000,
  pollInterval: 1_000,
});

if (!process.env.CINDER_BASE_URL && baseUrl) {
  process.env.CINDER_BASE_URL = baseUrl;
}

if (!process.env.CINDER_WORKER_NAME && workerName) {
  process.env.CINDER_WORKER_NAME = workerName;
}

if (!process.env.CINDER_FIXTURE_REPO && fixtureRepo) {
  process.env.CINDER_FIXTURE_REPO = fixtureRepo;
}

if (!process.env.CINDER_FIXTURE_BRANCH && fixtureBranch) {
  process.env.CINDER_FIXTURE_BRANCH = fixtureBranch;
}

if (!process.env.CINDER_FIXTURE_WORKFLOW && fixtureWorkflow) {
  process.env.CINDER_FIXTURE_WORKFLOW = fixtureWorkflow;
}

const scope = {
  spec: {
    title: "Cinder",
    tutorial: {
      goal: "Prove cinder on a live deployment, not just deploy it.",
      outcome:
        "Webhook intake, queueing, runner registration, cache paths, and the speed claim all go green.",
    },
    howTo: {
      task: "Run the cinder proof loop against already-provisioned infrastructure.",
      done:
        "Cinder only exits green when the live system can do the work and the speed claim holds.",
    },
    explanation: {
      summary:
        "alchemy.run.ts creates the infrastructure once and writes .gateproof/runtime.json. This file is only the acceptance loop for the live product.",
    },
  },
  plan: Plan.define({
    goals: [
      {
        id: "webhook",
        title: "A GitHub webhook queues a runnable job",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "GITHUB_PAT",
              "GITHUB_PAT is required to dispatch the GitHub proof workflow.",
            ),
            Require.env(
              "CINDER_FIXTURE_BRANCH",
              "Run bun run provision first or set CINDER_FIXTURE_BRANCH for the GitHub proof fixture.",
            ),
            Require.env(
              "CINDER_FIXTURE_WORKFLOW",
              "Run bun run provision first or set CINDER_FIXTURE_WORKFLOW for the GitHub proof fixture.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required for internal API access.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
          ],
          act: [
            Act.exec(
              `bun -e 'const repo = ${JSON.stringify(fixtureRepo)};
const workflow = ${JSON.stringify(fixtureWorkflow)};
const branch = ${JSON.stringify(fixtureBranch)};
const token = process.env.GITHUB_PAT;
if (!token) {
  throw new Error("GITHUB_PAT is required");
}
const headers = {
  Accept: "application/vnd.github+json",
  Authorization: "Bearer " + token,
  "X-GitHub-Api-Version": "2022-11-28",
};
const listUrl =
  "https://api.github.com/repos/" +
  repo +
  "/actions/workflows/" +
  workflow +
  "/runs?event=workflow_dispatch&branch=" +
  encodeURIComponent(branch) +
  "&per_page=20";
const response = await fetch(listUrl, { headers });
if (!response.ok) {
  throw new Error("GitHub workflow run listing failed: " + response.status);
}
const payload = await response.json();
const runs = Array.isArray(payload.workflow_runs) ? payload.workflow_runs : [];
for (const run of runs) {
  if (typeof run?.id !== "number" || run.status === "completed") {
    continue;
  }
  const cancelResponse = await fetch(
    "https://api.github.com/repos/" + repo + "/actions/runs/" + run.id + "/cancel",
    {
      method: "POST",
      headers,
    },
  );
  if (!cancelResponse.ok && cancelResponse.status !== 409) {
    throw new Error("GitHub workflow cancel failed: " + cancelResponse.status);
  }
}'`,
              {
                timeoutMs: 60_000,
              },
            ),
            Act.exec(
              `curl -sf -X POST https://api.github.com/repos/${fixtureRepo}/actions/workflows/${fixtureWorkflow}/dispatches \
                -H "Accept: application/vnd.github+json" \
                -H "Authorization: Bearer $GITHUB_PAT" \
                -H "X-GitHub-Api-Version: 2022-11-28" \
                -d '${JSON.stringify({ ref: fixtureBranch })}'`,
            ),
            Act.exec("sleep 5"),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("webhook_received"),
            Assert.hasAction("signature_verified"),
            Assert.hasAction("job_queued"),
          ],
          timeoutMs: 20_000,
        }),
      },
      {
        id: "queue",
        title: "A queued job can be inspected without dequeueing",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required for queue inspection.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
          ],
          act: [
            Act.exec(
              `sh -c 'curl -sf ${baseUrl}/jobs/peek \
                -H "Authorization: Bearer ${internalToken}" \
                | tee "${queuePayloadPath}"'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.responseBodyIncludes("repo_full_name"),
            Assert.responseBodyIncludes("repo_clone_url"),
            Assert.responseBodyIncludes("runner_registration_url"),
            Assert.responseBodyIncludes("runner_registration_token"),
            Assert.responseBodyIncludes("cache_key"),
          ],
          timeoutMs: 8_000,
        }),
      },
      {
        id: "runner",
        title: "A runner can execute a queued GitHub job",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required for runner registration.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
            Require.env(
              "GITHUB_PAT",
              "GITHUB_PAT is required to confirm the queued GitHub run completed.",
            ),
          ],
          act: [
            Act.exec(
              `curl -sf ${baseUrl}/jobs/peek \
                -H "Authorization: Bearer ${internalToken}" \
                > "${runnerJobPath}"`,
            ),
            Act.exec(
              `sh -c 'if [ -f "${agentPidPath}" ] && kill -0 "$(cat "${agentPidPath}")" 2>/dev/null; then exit 0; fi; : >"${agentLogPath}"; cargo run --quiet -p cinder-agent -- --url "${baseUrl}" --token "${internalToken}" --poll-ms 250 >"${agentLogPath}" 2>&1 & echo $! >"${agentPidPath}"; sleep 5'`,
            ),
            Act.exec(
              `bun -e 'import { existsSync, readFileSync } from "node:fs";
const payload = JSON.parse(readFileSync(${JSON.stringify(runnerJobPath)}, "utf8"));
if (typeof payload.run_id !== "number") {
  throw new Error("queue payload missing run_id");
}
if (typeof payload.repo_full_name !== "string" || payload.repo_full_name.length === 0) {
  throw new Error("queue payload missing repo_full_name");
}
const token = process.env.GITHUB_PAT;
if (!token) {
  throw new Error("GITHUB_PAT is required");
}
const headers = {
  Accept: "application/vnd.github+json",
  Authorization: "Bearer " + token,
  "X-GitHub-Api-Version": "2022-11-28",
};
const deadline = Date.now() + 600000;
let run = null;
while (Date.now() < deadline) {
  const response = await fetch(
    "https://api.github.com/repos/" + payload.repo_full_name + "/actions/runs/" + payload.run_id,
    { headers },
  );
  if (!response.ok) {
    throw new Error("GitHub workflow run fetch failed: " + response.status);
  }
  run = await response.json();
  if (run.status === "completed") {
    break;
  }
  await Bun.sleep(2000);
}
if (!run || run.status !== "completed") {
  throw new Error("GitHub workflow run did not complete");
}
const logNeedle = "completed with exit code 0";
const logDeadline = Date.now() + 30000;
while (Date.now() < logDeadline) {
  if (existsSync(${JSON.stringify(agentLogPath)})) {
    const logContents = readFileSync(${JSON.stringify(agentLogPath)}, "utf8");
    if (logContents.includes(logNeedle)) {
      break;
    }
  }
  await Bun.sleep(500);
}
console.log(JSON.stringify(run));
if (existsSync(${JSON.stringify(agentLogPath)})) {
  console.log(readFileSync(${JSON.stringify(agentLogPath)}, "utf8"));
}
if (run.conclusion !== "success") {
  process.exit(1);
}'`,
              {
                timeoutMs: 600_000,
              },
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("runner_registered"),
            Assert.hasAction("runner_pool_updated"),
            Assert.hasAction("job_dequeued"),
            Assert.responseBodyIncludes(`"conclusion":"success"`),
            Assert.responseBodyIncludes("starting github runner for job"),
            Assert.responseBodyIncludes("completed with exit code 0"),
          ],
          timeoutMs: 600_000,
        }),
      },
      {
        id: "cache-restore",
        title: "The fixture cache key currently restores as a cold miss",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required for cache restore.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
          ],
          act: [
            Act.exec(
              `bun -e 'import { readFileSync } from "node:fs";
const payload = JSON.parse(readFileSync(${JSON.stringify(queuePayloadPath)}, "utf8"));
if (typeof payload.cache_key !== "string" || payload.cache_key.length === 0) {
  throw new Error("queue payload missing cache_key");
}
const response = await fetch(
  ${JSON.stringify(baseUrl)} + "/cache/restore/" + payload.cache_key,
  {
    method: "POST",
    headers: {
      Authorization: "Bearer " + ${JSON.stringify(internalToken)},
    },
  },
);
if (!response.ok) {
  throw new Error("cache restore failed: " + response.status);
}
console.log(await response.text());'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("cache_miss"),
            Assert.responseBodyIncludes(`"miss":true`),
          ],
          timeoutMs: 5_000,
        }),
      },
      {
        id: "cache-push",
        title: "The cache upload path returns a usable upload URL",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required for cache upload.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
          ],
          act: [
            Act.exec(
              `curl -sf -X POST ${baseUrl}/cache/upload \
                -H "Content-Type: application/json" \
                -H "Authorization: Bearer ${internalToken}" \
                -d '{"key":"${newKey}","content_type":"application/x-tar","size_bytes":1024}'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("upload_url_generated"),
            Assert.responseBodyIncludes("http"),
          ],
          timeoutMs: 8_000,
        }),
      },
      {
        id: "speed-claim",
        title: "A warm build is materially faster than cold",
        gate: Gate.define({
          observe: workerLogs,
          prerequisites: [
            Require.env(
              "CLOUDFLARE_ACCOUNT_ID",
              "CLOUDFLARE_ACCOUNT_ID is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "CLOUDFLARE_API_TOKEN",
              "CLOUDFLARE_API_TOKEN is required for Cloudflare worker log observation.",
            ),
            Require.env(
              "COLD_BUILD_MS",
              "Set COLD_BUILD_MS to a real cold baseline in milliseconds.",
            ),
            Require.env(
              "TEST_REPO",
              "Set TEST_REPO to a real repository for the speed claim.",
            ),
          ],
          act: [
            Act.exec(
              `curl -sf -X POST ${harnessRunUrl} \
                -H "Content-Type: application/json" \
                -d '{"repo":"${testRepo}","with_cache":true}'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("build_complete"),
            Assert.numericDeltaFromEnv({
              source: "logMessage",
              pattern: "build_duration_ms=(\\d+)",
              baselineEnv: "COLD_BUILD_MS",
              minimumDelta: speedThresholdMs,
            }),
          ],
          timeoutMs: 120_000,
        }),
      },
    ],
    loop: {
      maxIterations: 1,
      stopOnFailure: true,
    },
    cleanup: {
      actions: [
        Act.exec(
          `if [ -n "${internalToken}" ] && [ -n "${baseUrl}" ]; then curl -sf -X DELETE ${baseUrl}/runners/${localRunnerId} -H "Authorization: Bearer ${internalToken}" >/dev/null; else exit 0; fi`,
        ),
      ],
    },
  }),
} satisfies ScopeFile;

export default scope;

if (import.meta.main) {
  if (testRepo) {
    await ensureLocalHarness();
  }

  await ensureColdBuildBaseline();

  try {
    const result = await Effect.runPromise(
      Plan.runLoop(scope.plan, {
        maxIterations: scope.plan.loop?.maxIterations,
      }),
    );

    console.log(JSON.stringify(result, null, 2));

    if (result.status !== "pass") {
      process.exitCode = 1;
    }
  } finally {
    stopManagedAgent();
    stopManagedHarness();
  }
}
