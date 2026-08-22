#!/usr/bin/env node
/**
 * Static editor + WebSocket epidemic relay.
 * The paper assumes reliable broadcast; this is that hop for distinct browsers.
 */
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocketServer } from "ws";

const root = path.resolve(fileURLToPath(new URL("../web", import.meta.url)));
const wasmArtifact = path.resolve(
  fileURLToPath(new URL("../target/wasm32-unknown-unknown/release/esbt.wasm", import.meta.url)),
);
const port = Number(process.env.PORT || 8080);
const rooms = new Map();

const mime = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".css": "text/css",
  ".json": "application/json",
};

const server = http.createServer((req, res) => {
  const u = new URL(req.url, "http://localhost");
  let p = decodeURIComponent(u.pathname);
  if (p === "/") p = "/index.html";
  const file = p === "/esbt.wasm" ? wasmArtifact : path.normalize(path.join(root, p));
  if (file !== wasmArtifact && !file.startsWith(root)) {
    res.writeHead(403);
    res.end();
    return;
  }
  fs.readFile(file, (err, data) => {
    if (err) {
      res.writeHead(404);
      res.end("not found");
      return;
    }
    res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" });
    res.end(data);
  });
});

const wss = new WebSocketServer({ noServer: true });
server.on("upgrade", (req, socket, head) => {
  const u = new URL(req.url, "http://localhost");
  if (u.pathname !== "/signal") {
    socket.destroy();
    return;
  }
  wss.handleUpgrade(req, socket, head, (ws) => {
    const room = u.searchParams.get("room") || "default";
    if (!rooms.has(room)) rooms.set(room, new Set());
    const peers = rooms.get(room);
    peers.add(ws);
    ws.on("message", (raw) => {
      for (const p of peers) {
        if (p !== ws && p.readyState === 1) p.send(raw.toString());
      }
    });
    ws.on("close", () => {
      peers.delete(ws);
      if (peers.size === 0) rooms.delete(room);
    });
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`esbt demo http://127.0.0.1:${port}`);
});
