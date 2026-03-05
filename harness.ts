import { readFileSync } from "node:fs";

type EnvMap = Record<string, string>;

function loadEnv(): EnvMap {
  const lines = readFileSync(new URL("./.env", import.meta.url), "utf8").split(/\r?\n/);
  const values: EnvMap = {};

  for (const line of lines) {
    if (!line || line.startsWith("#")) {
      continue;
    }

    const equalsIndex = line.indexOf("=");
    if (equalsIndex <= 0) {
      continue;
    }

    const key = line.slice(0, equalsIndex).trim();
    const value = line.slice(equalsIndex + 1).trim();
    if (key) {
      values[key] = value;
    }
  }

  return values;
}

const env = loadEnv();
const baseUrl = env.CINDER_BASE_URL;
const token = env.CINDER_INTERNAL_TOKEN;

if (!baseUrl) {
  throw new Error("CINDER_BASE_URL is required to run the local proof harness");
}

if (!token) {
  throw new Error("CINDER_INTERNAL_TOKEN is required to run the local proof harness");
}

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 9000,
  async fetch(request) {
    const url = new URL(request.url);

    if (request.method !== "POST" || url.pathname !== "/test/run") {
      return new Response("not found", { status: 404 });
    }

    const payload = await request.json();

    const response = await fetch(`${baseUrl}/test/build`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(payload),
    });

    const body = await response.text();

    return new Response(body, {
      status: response.status,
      headers: {
        "Content-Type": response.headers.get("Content-Type") ?? "application/json",
      },
    });
  },
});

console.log(`cinder harness listening on http://${server.hostname}:${server.port}/test/run`);
