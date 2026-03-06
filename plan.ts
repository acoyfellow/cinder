import { Effect } from "effect";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type { ScopeFile } from "gateproof";
import { Act, Assert, Gate, Plan, Require } from "gateproof";
import { Cloudflare } from "gateproof/cloudflare";

type RuntimeState = {
  orchestratorName?: string;
  orchestratorUrl?: string;
  proofTargetMode?: string;
  proofTargetRepo?: string;
  proofTargetBranch?: string;
  proofTargetWorkflow?: string;
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
      proofTargetMode:
        typeof parsed.proofTargetMode === "string" ? parsed.proofTargetMode : undefined,
      proofTargetRepo:
        typeof parsed.proofTargetRepo === "string" ? parsed.proofTargetRepo : undefined,
      proofTargetBranch:
        typeof parsed.proofTargetBranch === "string" ? parsed.proofTargetBranch : undefined,
      proofTargetWorkflow:
        typeof parsed.proofTargetWorkflow === "string" ? parsed.proofTargetWorkflow : undefined,
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

function stopManagedAgent(agentPidPath: string): void {
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

const runtimeState = loadRuntimeState();
const baseUrl = readOptionalEnv("CINDER_BASE_URL") ?? runtimeState?.orchestratorUrl ?? "";
const workerName =
  readOptionalEnv("CINDER_WORKER_NAME") ?? runtimeState?.orchestratorName ?? "cinder-orchestrator";
const proofTargetMode =
  readOptionalEnv("CINDER_PROOF_TARGET_MODE") ??
  runtimeState?.proofTargetMode ??
  "existing-repo";
const targetRepo =
  readOptionalEnv("CINDER_PROOF_TARGET_REPO") ??
  runtimeState?.proofTargetRepo ??
  readOptionalEnv("CINDER_FIXTURE_REPO") ??
  runtimeState?.fixtureRepo ??
  "acoyfellow/gateproof";
const targetBranch =
  readOptionalEnv("CINDER_PROOF_TARGET_BRANCH") ??
  runtimeState?.proofTargetBranch ??
  readOptionalEnv("CINDER_FIXTURE_BRANCH") ??
  runtimeState?.fixtureBranch ??
  "main";
const targetWorkflow =
  readOptionalEnv("CINDER_PROOF_TARGET_WORKFLOW") ??
  runtimeState?.proofTargetWorkflow ??
  readOptionalEnv("CINDER_FIXTURE_WORKFLOW") ??
  runtimeState?.fixtureWorkflow ??
  ".github/workflows/ci.yml";
const internalToken = readOptionalEnv("CINDER_INTERNAL_TOKEN") ?? "";
const demoUrl = readOptionalEnv("GATEPROOF_DEMO_URL") ?? "https://gateproof.dev";
const localRunnerId = resolveLocalRunnerId();
const agentLogPath = "/tmp/cinder-agent-proof.log";
const agentPidPath = "/tmp/cinder-agent-proof.pid";
const queuePayloadPath = "/tmp/cinder-proof-queue-payload.json";

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

if (!process.env.CINDER_PROOF_TARGET_MODE && proofTargetMode) {
  process.env.CINDER_PROOF_TARGET_MODE = proofTargetMode;
}

if (!process.env.CINDER_PROOF_TARGET_REPO && targetRepo) {
  process.env.CINDER_PROOF_TARGET_REPO = targetRepo;
}

if (!process.env.CINDER_PROOF_TARGET_BRANCH && targetBranch) {
  process.env.CINDER_PROOF_TARGET_BRANCH = targetBranch;
}

if (!process.env.CINDER_PROOF_TARGET_WORKFLOW && targetWorkflow) {
  process.env.CINDER_PROOF_TARGET_WORKFLOW = targetWorkflow;
}

const scope = {
  spec: {
    title: "Cinder",
    tutorial: {
      goal: "Prove cinder on a live deployment, not just on a fixture repo.",
      outcome:
        "Cinder only exits green when it can run Gateproof's real docs deploy workflow on a self-hosted Cinder runner.",
    },
    howTo: {
      task: "Provision Cinder against an existing proof target, then run the live proof loop.",
      done:
        "Webhook intake, queueing, runner execution, and the deployed docs smoke check all pass against the real Gateproof repo.",
    },
    explanation: {
      summary:
        "alchemy.run.ts provisions Cinder and wires a proof target repo. plan.ts reruns the live dogfood proof against that target.",
    },
  },
  plan: Plan.define({
    goals: [
      {
        id: "webhook",
        title: "A real Gateproof workflow_job webhook reaches Cinder",
        gate: Gate.define({
          prerequisites: [
            Require.env(
              "GITHUB_PAT",
              "GITHUB_PAT is required to dispatch the Gateproof workflow.",
            ),
            Require.env(
              "CINDER_INTERNAL_TOKEN",
              "CINDER_INTERNAL_TOKEN is required to clear stale queued jobs.",
            ),
            Require.env(
              "CINDER_BASE_URL",
              "Run bun run provision first or set CINDER_BASE_URL to the live orchestrator URL.",
            ),
          ],
          act: [
            Act.exec(
              `curl -sf ${baseUrl}/jobs/next -H "Authorization: Bearer ${internalToken}" >/dev/null || true`,
            ),
            Act.exec(
              `bun -e 'import { writeFileSync } from "node:fs";
const repo = ${JSON.stringify(targetRepo)};
const workflow = ${JSON.stringify(targetWorkflow)};
const branch = ${JSON.stringify(targetBranch)};
const token = process.env.GITHUB_PAT;
if (!token) {
  throw new Error("GITHUB_PAT is required");
}
const headers = {
  Accept: "application/vnd.github+json",
  Authorization: "Bearer " + token,
  "X-GitHub-Api-Version": "2022-11-28",
};
const encodedWorkflow = encodeURIComponent(workflow);
const listUrl =
  "https://api.github.com/repos/" +
  repo +
  "/actions/workflows/" +
  encodedWorkflow +
  "/runs?event=workflow_dispatch&branch=" +
  encodeURIComponent(branch) +
  "&per_page=20";
const listResponse = await fetch(listUrl, { headers });
if (!listResponse.ok) {
  throw new Error("GitHub workflow run listing failed: " + listResponse.status);
}
const listPayload = await listResponse.json();
const runs = Array.isArray(listPayload.workflow_runs) ? listPayload.workflow_runs : [];
const maxId = runs.reduce((highest, run) => {
  return typeof run?.id === "number" && run.id > highest ? run.id : highest;
}, 0);
writeFileSync("/tmp/cinder-proof-gateproof-before.txt", String(maxId));
writeFileSync("/tmp/cinder-proof-webhook-dispatch-start.txt", new Date().toISOString());
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
              `bun -e 'const repo = ${JSON.stringify(targetRepo)};
const workflow = ${JSON.stringify(targetWorkflow)};
const branch = ${JSON.stringify(targetBranch)};
const token = process.env.GITHUB_PAT;
if (!token) {
  throw new Error("GITHUB_PAT is required");
}
const response = await fetch(
  "https://api.github.com/repos/" +
    repo +
    "/actions/workflows/" +
    encodeURIComponent(workflow) +
    "/dispatches",
  {
    method: "POST",
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: "Bearer " + token,
      "X-GitHub-Api-Version": "2022-11-28",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ ref: branch }),
  },
);
if (!response.ok) {
  throw new Error("GitHub workflow dispatch failed: " + response.status);
}'`,
            ),
            Act.exec(
              `bun -e 'const repo = ${JSON.stringify(targetRepo)};
const token = process.env.GITHUB_PAT;
if (!token) {
  throw new Error("GITHUB_PAT is required");
}
const dispatchStartedAt = new Date(
  await Bun.file("/tmp/cinder-proof-webhook-dispatch-start.txt").text(),
);
if (Number.isNaN(dispatchStartedAt.getTime())) {
  throw new Error("missing webhook dispatch timestamp");
}
const headers = {
  Accept: "application/vnd.github+json",
  Authorization: "Bearer " + token,
  "X-GitHub-Api-Version": "2022-11-28",
};
const hooksResponse = await fetch("https://api.github.com/repos/" + repo + "/hooks", {
  headers,
});
if (!hooksResponse.ok) {
  throw new Error("GitHub webhook listing failed: " + hooksResponse.status);
}
const hooks = await hooksResponse.json();
if (!Array.isArray(hooks)) {
  throw new Error("GitHub webhook listing returned a non-array payload");
}
const hook = hooks.find((candidate) => {
  const events = Array.isArray(candidate?.events) ? candidate.events : [];
  return (
    candidate?.active === true &&
    typeof candidate?.id === "number" &&
    typeof candidate?.config?.url === "string" &&
    candidate.config.url.includes("/webhook/github") &&
    events.includes("workflow_job")
  );
});
if (!hook) {
  throw new Error("no active workflow_job webhook targeting Cinder was found");
}
const deadline = Date.now() + 300000;
while (Date.now() < deadline) {
  const deliveriesResponse = await fetch(
    "https://api.github.com/repos/" + repo + "/hooks/" + hook.id + "/deliveries?per_page=20",
    { headers },
  );
  if (!deliveriesResponse.ok) {
    throw new Error("GitHub webhook delivery listing failed: " + deliveriesResponse.status);
  }
  const deliveries = await deliveriesResponse.json();
  if (!Array.isArray(deliveries)) {
    throw new Error("GitHub webhook deliveries returned a non-array payload");
  }
  const matchingDelivery = deliveries.find((delivery) => {
    if (delivery?.event !== "workflow_job") {
      return false;
    }
    if (delivery?.status_code !== 200) {
      return false;
    }
    if (typeof delivery?.delivered_at !== "string") {
      return false;
    }
    const deliveredAt = new Date(delivery.delivered_at);
    return !Number.isNaN(deliveredAt.getTime()) && deliveredAt >= dispatchStartedAt;
  });
  if (matchingDelivery) {
    console.log(JSON.stringify(matchingDelivery));
    process.exit(0);
  }
  await Bun.sleep(2000);
}
throw new Error("no successful workflow_job webhook delivery was observed after dispatch");'`,
              {
                timeoutMs: 300_000,
              },
            ),
          ],
          assert: [
            Assert.noErrors(),
          ],
          timeoutMs: 600_000,
        }),
      },
      {
        id: "queue",
        title: "The queued Gateproof deploy job is execution-ready",
        gate: Gate.define({
          prerequisites: [
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
              `bun -e 'import { writeFileSync } from "node:fs";
const baseUrl = ${JSON.stringify(baseUrl)};
const token = ${JSON.stringify(internalToken)};
const targetRepo = ${JSON.stringify(targetRepo)};
const outputPath = ${JSON.stringify(queuePayloadPath)};
const deadline = Date.now() + 600000;
while (Date.now() < deadline) {
  const response = await fetch(baseUrl + "/jobs/peek", {
    headers: {
      Authorization: "Bearer " + token,
    },
  });
  if (!response.ok) {
    throw new Error("queue peek failed: " + response.status);
  }
  const payload = await response.json();
  const labels = Array.isArray(payload.labels) ? payload.labels : [];
  const matchesRepo = payload.repo_full_name === targetRepo;
  const matchesLabels = labels.includes("self-hosted") && labels.includes("cinder");
  if (
    matchesRepo &&
    matchesLabels &&
    typeof payload.job_id === "number" &&
    typeof payload.runner_registration_token === "string" &&
    payload.runner_registration_token.length > 0
  ) {
    writeFileSync(outputPath, JSON.stringify(payload, null, 2));
    console.log(JSON.stringify(payload));
    process.exit(0);
  }
  await Bun.sleep(2000);
}
throw new Error("no queued Gateproof deploy job became available");'`,
              {
                timeoutMs: 600_000,
              },
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.responseBodyIncludes(`"repo_full_name":"${targetRepo}"`),
            Assert.responseBodyIncludes(`"runner_registration_url":"https://github.com/${targetRepo}"`),
            Assert.responseBodyIncludes(`"runner_registration_token":"`),
          ],
          timeoutMs: 600_000,
        }),
      },
      {
        id: "runner",
        title: "The local cinder-agent runs Gateproof's real deploy job",
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
              `sh -c 'if [ -f "${agentPidPath}" ] && kill -0 "$(cat "${agentPidPath}")" 2>/dev/null; then exit 0; fi; : >"${agentLogPath}"; cargo run --quiet -p cinder-agent -- --url "${baseUrl}" --token "${internalToken}" --poll-ms 250 >"${agentLogPath}" 2>&1 & echo $! >"${agentPidPath}"; sleep 5'`,
            ),
            Act.exec(
              `bun -e 'import { existsSync, readFileSync } from "node:fs";
const payload = JSON.parse(readFileSync(${JSON.stringify(queuePayloadPath)}, "utf8"));
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
const deadline = Date.now() + 1800000;
let run = null;
while (Date.now() < deadline) {
  const response = await fetch(
    "https://api.github.com/repos/" + payload.repo_full_name + "/actions/runs/" + payload.run_id,
    { headers },
  );
  if (!response.ok) {
    if (response.status >= 500) {
      await Bun.sleep(2000);
      continue;
    }
    throw new Error("GitHub workflow run fetch failed: " + response.status);
  }
  run = await response.json();
  if (run.status === "completed") {
    break;
  }
  await Bun.sleep(5000);
}
if (!run || run.status !== "completed") {
  throw new Error("GitHub workflow run did not complete");
}
const logNeedles = ["starting github runner for job", "completed with exit code 0"];
const logDeadline = Date.now() + 30000;
while (Date.now() < logDeadline) {
  if (existsSync(${JSON.stringify(agentLogPath)})) {
    const logContents = readFileSync(${JSON.stringify(agentLogPath)}, "utf8");
    if (logNeedles.every((needle) => logContents.includes(needle))) {
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
                timeoutMs: 1_800_000,
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
          timeoutMs: 1_800_000,
        }),
      },
      {
        id: "deploy-smoke",
        title: "The deployed Gateproof docs site is healthy after the run",
        gate: Gate.define({
          act: [
            Act.exec(
              `bun -e 'const baseUrl = ${JSON.stringify(demoUrl)};
const checks = [
  {
    path: "/",
    needle: "Build software in reverse.",
  },
  {
    path: "/case-studies",
    needle: "Historical validation records.",
  },
  {
    path: "/case-studies/cinder",
    needle: "Chapter 2: Gateproof docs dogfood proof",
  },
];

for (const check of checks) {
  const response = await fetch(baseUrl + check.path, { redirect: "follow" });
  const body = await response.text();

  if (!response.ok) {
    throw new Error(
      "smoke failed for " + check.path + ": expected 200 but observed " + response.status,
    );
  }

  if (!body.includes(check.needle)) {
    throw new Error(
      "smoke failed for " +
        check.path +
        ": response missing marker " +
        JSON.stringify(check.needle),
    );
  }

  console.log("smoke ok " + check.path + " " + response.status);
}'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.responseBodyIncludes("smoke ok / 200"),
            Assert.responseBodyIncludes("smoke ok /case-studies 200"),
            Assert.responseBodyIncludes("smoke ok /case-studies/cinder 200"),
          ],
          timeoutMs: 30_000,
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
  stopManagedAgent(agentPidPath);
  rmSync(queuePayloadPath, { force: true });

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
    stopManagedAgent(agentPidPath);
  }

  process.exit(process.exitCode ?? 0);
}
