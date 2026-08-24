import assert from "node:assert/strict";
import test from "node:test";
import { ExactSeenSet, Mesh } from "../web/mesh.js";

test("exact artifact dedup does not suppress a retired FNV-1a collision", () => {
  const left = Uint8Array.from(Buffer.from("942946c7e307045f", "hex"));
  const right = Uint8Array.from(Buffer.from("023555717970131e", "hex"));
  assert.equal(retiredFnv(left), retiredFnv(right), "fixture must collide under the retired hash");

  const seen = new ExactSeenSet();
  assert.equal(seen.admit(`artifact:${Buffer.from(left).toString("base64")}`), true);
  assert.equal(seen.admit(`artifact:${Buffer.from(right).toString("base64")}`), true);
  assert.equal(seen.admit(`artifact:${Buffer.from(left).toString("base64")}`), false);
});

test("bounded exact dedup evicts instead of creating false positive matches", () => {
  const seen = new ExactSeenSet(2, 1_000);
  assert.equal(seen.admit("one"), true);
  assert.equal(seen.admit("two"), true);
  assert.equal(seen.admit("three"), true);
  assert.equal(seen.admit("one"), true, "evicted bytes may be retried idempotently");
});

test("version anti-entropy exports a gap-aware delta", () => {
  const remote = Uint8Array.from([1, 2, 3]);
  const delta = Uint8Array.from([4, 5]);
  let received;
  let sent;
  const mesh = Object.create(Mesh.prototype);
  mesh.antiEntropy = {
    getVersion: () => new Uint8Array(),
    makeDelta(version) {
      received = version;
      return delta;
    },
    makeSnapshot() {
      throw new Error("snapshot must not be used");
    },
  };
  mesh.gossip = (bytes) => {
    sent = bytes;
  };

  mesh._answerVersion(Buffer.from(remote).toString("base64"));

  assert.deepEqual(received, remote);
  assert.equal(sent, delta);
});

test("history-floor refusal uses one canonical snapshot fallback", () => {
  const snapshot = Uint8Array.from([9, 8, 7]);
  let sent;
  const mesh = Object.create(Mesh.prototype);
  mesh.antiEntropy = {
    getVersion: () => new Uint8Array(),
    makeDelta() {
      throw { code: 21 };
    },
    makeSnapshot() {
      return snapshot;
    },
  };
  mesh.gossip = (bytes) => {
    sent = bytes;
  };

  mesh._answerVersion(Buffer.from([1]).toString("base64"));

  assert.equal(sent, snapshot);
});

function retiredFnv(bytes) {
  let hash = 2_166_136_261;
  for (const byte of bytes) {
    hash ^= byte;
    hash = Math.imul(hash, 16_777_619);
  }
  return hash >>> 0;
}
