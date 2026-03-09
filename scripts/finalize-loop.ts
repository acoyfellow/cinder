import { existsSync, readFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

type LoopBaseState = {
  branch: string;
  startHead: string;
  planPath: string;
  timestamp: string;
};

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const loopBasePath = resolve(repoRoot, ".gateproof", "loop-base.json");

function run(command: string, args: string[], options: { capture?: boolean } = {}): string {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit",
    env: process.env,
  });

  if (result.status !== 0) {
    const message =
      (result.stderr ?? "").trim() ||
      (result.stdout ?? "").trim() ||
      `${command} ${args.join(" ")} failed`;
    throw new Error(message);
  }

  return (result.stdout ?? "").trim();
}

function loadLoopBaseState(): LoopBaseState {
  if (!existsSync(loopBasePath)) {
    throw new Error(`missing ${loopBasePath}; there is no active worker loop to finalize`);
  }

  const parsed: unknown = JSON.parse(readFileSync(loopBasePath, "utf8"));
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    typeof (parsed as LoopBaseState).branch !== "string" ||
    typeof (parsed as LoopBaseState).startHead !== "string" ||
    typeof (parsed as LoopBaseState).planPath !== "string" ||
    typeof (parsed as LoopBaseState).timestamp !== "string"
  ) {
    throw new Error(`invalid loop base state in ${loopBasePath}`);
  }

  return parsed as LoopBaseState;
}

function main() {
  const commitMessage = process.argv.slice(2).join(" ").trim();
  if (!commitMessage) {
    throw new Error('usage: bun run prove:finalize -- "<commit message>"');
  }

  const state = loadLoopBaseState();
  const branch = run("git", ["branch", "--show-current"], { capture: true });
  if (branch !== state.branch) {
    throw new Error(`loop base was created on ${state.branch}, but current branch is ${branch}`);
  }

  const status = run("git", ["status", "--porcelain"], { capture: true });
  if (status.length > 0) {
    throw new Error("working tree must be clean before prove:finalize");
  }

  run("bun", ["run", "prove:once"]);

  const head = run("git", ["rev-parse", "HEAD"], { capture: true });
  if (head === state.startHead) {
    rmSync(loopBasePath, { force: true });
    throw new Error("no checkpoint commits were created after the loop baseline");
  }

  run("git", ["reset", "--soft", state.startHead]);
  run("git", ["commit", "-m", commitMessage]);
  rmSync(loopBasePath, { force: true });
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
