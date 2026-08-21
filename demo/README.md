# Three-harness ESBT showcase

Playwright, Puppeteer, and [agent-browser](https://github.com/vercel-labs/agent-browser)
each drive a separate Chrome against the same `?room=`.

Signaling is a WebSocket epidemic hop (`demo/server.mjs`). Chrome is the
system binary at `/opt/google/chrome/chrome`.

## Scenarios

1. Paper Situations 1–3 + Alg. 3 + SEC (in-Wasm tests)
2. Empty start converges
3. Three concurrent appends (Alpha / Bravo / Charlie)
4. Causal delete seen by every harness
5. Late joiner snapshot
6. Interleaved burst (`+++++` / `-----` / `*****`)

## Run

```bash
# deps live in /tmp if the artifacts volume is picky
PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 PUPPETEER_SKIP_DOWNLOAD=1 \
  npm install --prefix /tmp/esbt-npm

NODE_PATH=/tmp/esbt-npm/node_modules \
AGENT_BROWSER=/tmp/esbt-npm/node_modules/.bin/agent-browser \
CHROME_PATH=/opt/google/chrome/chrome \
node demo/showcase.mjs
```
