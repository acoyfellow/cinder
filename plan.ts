import { Effect } from "effect";
import crypto from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
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
    };
  } catch {
    return null;
  }
}

const runtimeState = loadRuntimeState();
const baseUrl = readOptionalEnv("CINDER_BASE_URL") ?? runtimeState?.orchestratorUrl ?? "";
const workerName =
  readOptionalEnv("CINDER_WORKER_NAME") ?? runtimeState?.orchestratorName ?? "cinder-orchestrator";
const internalToken = readOptionalEnv("CINDER_INTERNAL_TOKEN") ?? "";
const webhookSecret = readOptionalEnv("GITHUB_WEBHOOK_SECRET") ?? "";

const missKey = crypto.randomBytes(32).toString("hex");
const newKey = crypto.randomBytes(32).toString("hex");
const speedThresholdMs = Number(process.env.SPEED_THRESHOLD_MS ?? "60000");
const testRepo = process.env.TEST_REPO ?? "";

async function ensureColdBuildBaseline(): Promise<void> {
  if (readOptionalEnv("COLD_BUILD_MS")) {
    return;
  }

  if (!testRepo) {
    return;
  }

  try {
    const response = await fetch("http://localhost:9000/test/run", {
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

function githubSignature(payload: string, secret: string): string {
  return (
    "sha256=" +
    crypto.createHmac("sha256", secret).update(payload).digest("hex")
  );
}

const webhookPayload = JSON.stringify({
  action: "queued",
  workflow_job: {
    id: 99991,
    run_id: 99991,
    name: "cinder-plan-test",
    labels: ["self-hosted", "cinder"],
  },
  repository: {
    full_name: "acoyfellow/cinder-prd-test",
  },
});

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
              "GITHUB_WEBHOOK_SECRET",
              "GITHUB_WEBHOOK_SECRET is required for webhook verification.",
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
              `curl -sf -X POST ${baseUrl}/webhook/github \
                -H "Content-Type: application/json" \
                -H "X-Hub-Signature-256: ${githubSignature(webhookPayload, webhookSecret)}" \
                -d '${webhookPayload}'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("webhook_received"),
            Assert.hasAction("signature_verified"),
            Assert.hasAction("job_queued"),
          ],
          timeoutMs: 10_000,
        }),
      },
      {
        id: "queue",
        title: "A queued job can be dequeued",
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
              `curl -sf ${baseUrl}/jobs/next \
                -H "Authorization: Bearer ${internalToken}"`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("job_dequeued"),
            Assert.responseBodyIncludes("run_id"),
            Assert.responseBodyIncludes("labels"),
          ],
          timeoutMs: 8_000,
        }),
      },
      {
        id: "runner",
        title: "A runner can register into the pool",
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
          ],
          act: [
            Act.exec(
              `curl -sf -X POST ${baseUrl}/runners/register \
                -H "Content-Type: application/json" \
                -H "Authorization: Bearer ${internalToken}" \
                -d '{"runner_id":"plan-test-runner","labels":["self-hosted","cinder"],"arch":"x86_64"}'`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("runner_registered"),
            Assert.hasAction("runner_pool_updated"),
          ],
          timeoutMs: 8_000,
        }),
      },
      {
        id: "cache-restore",
        title: "A missing cache key returns a clean miss",
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
              `curl -sf -X POST ${baseUrl}/cache/restore/${missKey} \
                -H "Authorization: Bearer ${internalToken}"`,
            ),
          ],
          assert: [
            Assert.noErrors(),
            Assert.hasAction("cache_miss"),
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
              `curl -sf -X POST http://localhost:9000/test/run \
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
          `if [ -n "${internalToken}" ] && [ -n "${baseUrl}" ]; then curl -sf -X DELETE ${baseUrl}/runners/plan-test-runner -H "Authorization: Bearer ${internalToken}" >/dev/null; else exit 0; fi`,
        ),
      ],
    },
  }),
} satisfies ScopeFile;

export default scope;

if (import.meta.main) {
  const result = await Effect.runPromise(
    Plan.runLoop(scope.plan, {
      maxIterations: scope.plan.loop?.maxIterations,
    }),
  );

  console.log(JSON.stringify(result, null, 2));

  if (result.status !== "pass") {
    process.exitCode = 1;
  }
}
