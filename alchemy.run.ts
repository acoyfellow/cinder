import alchemy from "alchemy";
import { mkdir } from "node:fs/promises";
import {
  DurableObjectNamespace,
  KVNamespace,
  R2Bucket,
  Worker,
} from "alchemy/cloudflare";

export const app = await alchemy("cinder", {
  stage: process.env.CINDER_STAGE ?? "production",
});

export const cacheBucket = await R2Bucket("cinder-cache", {
  empty: false,
});

export const runnerState = await KVNamespace("cinder-runner-state");

export const runnerPool = await DurableObjectNamespace("RunnerPool", {
  className: "RunnerPool",
  sqlite: true,
});

export const jobQueue = await DurableObjectNamespace("JobQueue", {
  className: "JobQueue",
  sqlite: true,
});

export const orchestrator = await Worker("cinder-orchestrator", {
  entrypoint: "./crates/cinder-orchestrator/build/worker/shim.mjs",
  bindings: {
    CACHE_BUCKET: cacheBucket,
    RUNNER_STATE: runnerState,
    RUNNER_POOL: runnerPool,
    JOB_QUEUE: jobQueue,
    GITHUB_WEBHOOK_SECRET: alchemy.secret(process.env.GITHUB_WEBHOOK_SECRET!),
    CINDER_INTERNAL_TOKEN: alchemy.secret(process.env.CINDER_INTERNAL_TOKEN!),
  },
});

export const cacheWorker = await Worker("cinder-cache", {
  entrypoint: "./crates/cinder-cache/build/worker/shim.mjs",
  bindings: {
    CACHE_BUCKET: cacheBucket,
    CINDER_INTERNAL_TOKEN: alchemy.secret(process.env.CINDER_INTERNAL_TOKEN!),
  },
});

await app.finalize();

const runtimeDirectory = new URL("./.gateproof/", import.meta.url);
const runtimeFile = new URL("./.gateproof/runtime.json", import.meta.url);

await mkdir(runtimeDirectory, { recursive: true });

await Bun.write(
  runtimeFile,
  `${JSON.stringify(
    {
      generatedAt: new Date().toISOString(),
      stage: process.env.CINDER_STAGE ?? "production",
      orchestratorName: orchestrator.name,
      orchestratorUrl: orchestrator.url,
      cacheWorkerName: cacheWorker.name,
      cacheWorkerUrl: cacheWorker.url,
    },
    null,
    2,
  )}\n`,
);

console.log(`Wrote runtime outputs to ${runtimeFile.pathname}`);
