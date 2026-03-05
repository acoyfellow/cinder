#!/usr/bin/env bun
/**
 * Orchestrates the full Cinder proof: provision (if needed), run plan, print results.
 * Run from cinder repo root: bun run demo
 */

import { existsSync, readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");

function readEnv(): Record<string, string> {
  const path = resolve(repoRoot, ".env");
  if (!existsSync(path)) return {};
  const content = readFileSync(path, "utf8");
  const out: Record<string, string> = {};
  for (const line of content.split(/\r?\n/)) {
    if (!line || line.startsWith("#")) continue;
    const eq = line.indexOf("=");
    if (eq <= 0) continue;
    const key = line.slice(0, eq).trim();
    const val = line.slice(eq + 1).trim();
    if (key) out[key] = val;
  }
  return out;
}

function hasRuntimeJson(): boolean {
  return existsSync(resolve(repoRoot, ".gateproof/runtime.json"));
}

function checkPrereqs(): { ok: boolean; missing: string[] } {
  const env = { ...process.env, ...readEnv() };
  const required = [
    "CLOUDFLARE_ACCOUNT_ID",
    "CLOUDFLARE_API_TOKEN",
    "GITHUB_PAT",
    "GITHUB_WEBHOOK_SECRET",
    "CINDER_INTERNAL_TOKEN",
  ];
  const missing = required.filter((k) => !env[k]?.trim());
  return { ok: missing.length === 0, missing };
}

function run(cmd: string, args: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolvePromise) => {
    const proc = spawn(cmd, args, {
      cwd: repoRoot,
      stdio: ["inherit", "pipe", "pipe"],
      shell: true,
    });
    let stdout = "";
    let stderr = "";
    proc.stdout?.on("data", (d) => { stdout += d.toString(); });
    proc.stderr?.on("data", (d) => { stderr += d.toString(); });
    proc.on("close", (code) => {
      resolvePromise({ code: code ?? 1, stdout, stderr });
    });
  });
}

function parsePlanOutput(stdout: string): { result?: unknown; raw: string } {
  const marker = '"goals":';
  const idx = stdout.lastIndexOf(marker);
  if (idx < 0) return { raw: stdout };
  let start = idx;
  while (start > 0 && stdout[start] !== "{") start--;
  if (stdout[start] !== "{") return { raw: stdout };
  let depth = 0;
  let end = start;
  for (let i = start; i < stdout.length; i++) {
    if (stdout[i] === "{") depth++;
    if (stdout[i] === "}") {
      depth--;
      if (depth === 0) {
        end = i + 1;
        break;
      }
    }
  }
  try {
    const parsed = JSON.parse(stdout.slice(start, end)) as unknown;
    if (parsed && typeof parsed === "object" && "status" in parsed && "goals" in parsed) {
      return { result: parsed, raw: stdout };
    }
  } catch {
    // ignore
  }
  return { raw: stdout };
}

function printSummary(result: { status: string; goals?: Array<{ id: string; status: string; summary?: string }>; summary?: string }) {
  console.log("\n--- Cinder demo summary ---");
  console.log("Status:", result.status);
  if (result.summary) console.log("Summary:", result.summary);
  if (result.goals?.length) {
    console.log("\nGates:");
    for (const g of result.goals) {
      console.log(`  ${g.id}: ${g.status}${g.summary ? ` (${g.summary})` : ""}`);
    }
  }
  console.log("----------------------------\n");
}

async function main() {
  const provisionOnly = process.argv.includes("--provision-only");
  const proveOnly = process.argv.includes("--prove-only");

  const prereqs = checkPrereqs();
  if (!prereqs.ok) {
    console.error("Missing required env vars:", prereqs.missing.join(", "));
    console.error("Copy .env.example to .env and fill in values. See examples/README.md");
    process.exit(1);
  }

  if (!proveOnly && !hasRuntimeJson()) {
    console.log("No .gateproof/runtime.json found. Provisioning...");
    const prov = await run("bun", ["run", "provision"]);
    if (prov.code !== 0) {
      console.error("Provision failed:", prov.stderr || prov.stdout);
      process.exit(1);
    }
    console.log("Provision complete.");
    if (provisionOnly) {
      console.log("Use bun run demo (without --provision-only) to run the proof.");
      process.exit(0);
    }
  }

  if (provisionOnly) {
    console.log(hasRuntimeJson() ? "Already provisioned (.gateproof/runtime.json exists)." : "Provision complete.");
    process.exit(0);
  }

  console.log("Running Cinder proof (plan.ts)...");
  console.log("This may take 10-15 minutes.\n");

  const out = await run("bun", ["plan.ts"]);
  const { result, raw } = parsePlanOutput(out.stdout);

  if (out.stderr) process.stderr.write(out.stderr);

  if (result) {
    printSummary(result as Parameters<typeof printSummary>[0]);
  } else {
    console.log(raw.slice(-2000));
  }

  process.exit(out.code);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
