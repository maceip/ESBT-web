import { spawn } from "node:child_process";
import { CHROME, CHROME_ARGS } from "./chrome.mjs";

const SESSION = "esbt-agent";

function run(args, { json = true } = {}) {
  return new Promise((resolve, reject) => {
    const bin =
      process.env.AGENT_BROWSER ||
      new URL("../node_modules/.bin/agent-browser", import.meta.url).pathname;
    const full = [
      "--session",
      SESSION,
      "--executable-path",
      CHROME,
      "--args",
      CHROME_ARGS.join(","),
      ...(json ? ["--json"] : []),
      ...args,
    ];
    const child = spawn(bin, full, {
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        AGENT_BROWSER_EXECUTABLE_PATH: CHROME,
        AGENT_BROWSER_SESSION: SESSION,
      },
    });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("close", (code) => {
      if (code !== 0) {
        reject(new Error(`agent-browser ${args[0]} exit ${code}: ${err || out}`));
        return;
      }
      resolve(out);
    });
  });
}

function unwrap(raw) {
  let v = JSON.parse(raw);
  if (v && typeof v === "object" && "data" in v) {
    v = v.data?.result ?? v.data ?? v;
  }
  if (typeof v === "string") {
    try {
      v = JSON.parse(v);
    } catch (_) {}
  }
  return v;
}

export async function launchAgentBrowser(url) {
  await run(["open", url], { json: false });
  for (let i = 0; i < 40; i++) {
    try {
      const raw = await run(["eval", "!!window.__esbtDemo"]);
      if (String(raw).includes("true")) break;
    } catch (_) {}
    await new Promise((r) => setTimeout(r, 200));
  }
  return {
    name: "agent-browser",
    async type(text) {
      await run(["click", "#doc"], { json: false });
      await run(["type", "#doc", text], { json: false });
    },
    async backspace(n) {
      await run(["click", "#doc"], { json: false });
      for (let i = 0; i < n; i++) {
        await run(["press", "Backspace"], { json: false });
      }
    },
    async state() {
      const raw = await run([
        "eval",
        "JSON.stringify({text:window.__esbtDemo.text(),hash:window.__esbtDemo.hash(),len:window.__esbtDemo.len(),pending:window.__esbtDemo.pending(),site:window.__esbtDemo.site})",
      ]);
      return unwrap(raw);
    },
    async verify() {
      const raw = await run(["eval", "JSON.stringify(window.__esbtDemo.verify())"]);
      return unwrap(raw);
    },
    async close() {
      try {
        await run(["close"], { json: false });
      } catch (_) {}
    },
  };
}
