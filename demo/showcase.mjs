#!/usr/bin/env node
/**
 * Three-browser ESBT showcase.
 * Playwright, Puppeteer, and agent-browser each drive a distinct Chrome
 * against the same room. WebSocket signaling is the epidemic hop.
 */
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { launchPlaywright } from "./harness-playwright.mjs";
import { launchPuppeteer } from "./harness-puppeteer.mjs";
import { launchAgentBrowser } from "./harness-agent-browser.mjs";

const PORT = Number(process.env.PORT || 8765);
const ROOM = process.env.ESBT_ROOM || `demo-${Date.now().toString(36)}`;
const BASE = `http://127.0.0.1:${PORT}/?room=${ROOM}`;

function startServer() {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["demo/server.mjs"], {
      cwd: new URL("..", import.meta.url),
      env: { ...process.env, PORT: String(PORT) },
      stdio: ["ignore", "pipe", "pipe"],
    });
    child.stdout.on("data", (d) => {
      if (String(d).includes("esbt demo")) resolve(child);
    });
    child.stderr.on("data", (d) => process.stderr.write(d));
    child.on("error", reject);
    setTimeout(() => resolve(child), 1500);
  });
}

async function waitConverged(users, { timeout = 15000 } = {}) {
  const t0 = Date.now();
  let last = null;
  while (Date.now() - t0 < timeout) {
    const states = [];
    for (const u of users) states.push(await u.state());
    last = states;
    const hashes = states.map((s) => s.hash);
    const texts = states.map((s) => s.text);
    if (hashes.every((h) => h === hashes[0]) && texts.every((t) => t === texts[0])) {
      return states;
    }
    await sleep(250);
  }
  throw new Error("did not converge: " + JSON.stringify(last, null, 2));
}

function ok(name, detail) {
  console.log(`PASS  ${name}${detail ? " — " + detail : ""}`);
}

function fail(name, err) {
  console.error(`FAIL  ${name} — ${err.message || err}`);
  throw err;
}

async function scenario(name, fn) {
  process.stdout.write(`\n▸ ${name}\n`);
  try {
    await fn();
  } catch (e) {
    fail(name, e);
  }
}

async function main() {
  const server = await startServer();
  let users = [];
  try {
    console.log(`room ${ROOM}`);
    console.log("launching Playwright / Puppeteer / agent-browser …");
    users = await Promise.all([
      launchPlaywright(BASE),
      launchPuppeteer(BASE),
      launchAgentBrowser(BASE),
    ]);
    console.log("sites:", (await Promise.all(users.map((u) => u.state()))).map((s) => `${s.site}@${s.hash}`).join("  "));

    await scenario("empty start converges", async () => {
      const s = await waitConverged(users);
      if (s[0].text !== "") throw new Error("expected empty");
      ok("three empty replicas", `hash=${s[0].hash}`);
    });

    await scenario("three concurrent appends (identifier density)", async () => {
      await Promise.all([
        users[0].type("Alpha"),
        users[1].type("Bravo"),
        users[2].type("Charlie"),
      ]);
      const s = await waitConverged(users);
      const t = s[0].text;
      const bag = (x) => [...x].sort().join("");
      const expect = bag("AlphaBravoCharlie");
      if (bag(t) !== expect) {
        throw new Error(`bag ${bag(t)} != ${expect} text=${JSON.stringify(t)}`);
      }
      ok(
        "SEC under concurrent same-slot inserts",
        `n=${t.length} hash=${s[0].hash} text=${JSON.stringify(t)}`
      );
    });

    await scenario("causal delete observed by every harness", async () => {
      const before = (await users[0].state()).len;
      await users[0].backspace(1);
      const s = await waitConverged(users);
      if (s[0].len !== before - 1) throw new Error(`len ${s[0].len} expected ${before - 1}`);
      ok("delete replicated", `n=${s[0].len} hash=${s[0].hash}`);
    });

    await scenario("late joiner via snapshot (fourth Playwright)", async () => {
      const extra = await launchPlaywright(BASE + "&late=1");
      users.push(extra);
      try {
        const s = await waitConverged(users, { timeout: 20000 });
        ok("late join matches live hash", `n=${s[0].len} hash=${s[0].hash}`);
      } finally {
        await extra.close();
        users.pop();
      }
    });

    await scenario("interleaved paste-scale burst", async () => {
      await Promise.all([
        users[0].type("+++++"),
        users[1].type("-----"),
        users[2].type("*****"),
      ]);
      const s = await waitConverged(users);
      const plus = (s[0].text.match(/\+/g) || []).length;
      const minus = (s[0].text.match(/-/g) || []).length;
      const star = (s[0].text.match(/\*/g) || []).length;
      if (plus < 5 || minus < 5 || star < 5) {
        throw new Error(s[0].text);
      }
      ok("burst still SEC", `n=${s[0].len} hash=${s[0].hash}`);
    });

    console.log("\nAll showcase scenarios passed.");
  } finally {
    for (const u of users) {
      try {
        await u.close();
      } catch (_) {}
    }
    server.kill("SIGTERM");
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
