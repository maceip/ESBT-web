# Three-harness ESBT showcase

Playwright, Puppeteer, and [agent-browser](https://github.com/vercel-labs/agent-browser)
each drive a separate Chrome against the same `?room=`.

Signaling is a WebSocket epidemic hop (`demo/server.mjs`). This relay is test
and demonstration infrastructure, not the production WebSocket server. Chrome
is the system binary at `/opt/google/chrome/chrome`.

## Scenarios

1. Empty start converges
2. Three concurrent appends (Alpha / Bravo / Charlie)
3. Causal delete seen by every harness
4. Late joiner snapshot
5. Interleaved burst (`+++++` / `-----` / `*****`)

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
