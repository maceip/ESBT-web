/**
 * Epidemic demo mesh with explicit anti-entropy.
 *
 * Durable CRDT artifacts are deduplicated by their exact base64 bytes, never
 * by a lossy application hash. The cache is resource-bounded: eviction may
 * cause an idempotent artifact to be applied again, but can never suppress a
 * distinct artifact as a hash collision would.
 */

const STUN = [{ urls: "stun:stun.l.google.com:19302" }];
const MAX_SEEN_ENTRIES = 4_096;
const MAX_SEEN_KEY_CHARS = 64 * 1024 * 1024;
const MAX_WS_QUEUE_ENTRIES = 256;
const MAX_WS_QUEUE_CHARS = 16 * 1024 * 1024;
const ANTI_ENTROPY_INTERVAL_MS = 10_000;
const MAX_RECONNECT_MS = 15_000;

export class ExactSeenSet {
  constructor(maxEntries = MAX_SEEN_ENTRIES, maxKeyChars = MAX_SEEN_KEY_CHARS) {
    this.maxEntries = maxEntries;
    this.maxKeyChars = maxKeyChars;
    this.keys = new Map();
    this.keyChars = 0;
  }

  admit(key) {
    if (this.keys.has(key)) return false;
    this.keys.set(key, key.length);
    this.keyChars += key.length;
    while (this.keys.size > this.maxEntries || this.keyChars > this.maxKeyChars) {
      const oldest = this.keys.entries().next().value;
      if (!oldest) break;
      this.keys.delete(oldest[0]);
      this.keyChars -= oldest[1];
    }
    return true;
  }
}

export class Mesh {
  constructor({ room, onBytes, onPeer }) {
    this.room = room;
    this.id = crypto.randomUUID();
    this.onBytes = onBytes;
    this.onPeer = onPeer;
    this.peers = new Map();
    this.seenArtifacts = new ExactSeenSet();
    this.seenControl = new ExactSeenSet(8_192, 2 * 1024 * 1024);
    this.wsQueue = [];
    this.wsQueueChars = 0;
    this.ws = null;
    this.wsAttempt = 0;
    this.wsReconnectTimer = null;
    this.closed = false;
    this.controlSequence = 0;
    this.antiEntropy = null;

    this.ch = new BroadcastChannel(`esbt:${room}`);
    this.ch.onmessage = (event) => this._take(event.data, "tab");
    this._connectWs();
    this._publishControl("hello");
    this.antiEntropyTimer = setInterval(
      () => this.advertise(),
      ANTI_ENTROPY_INTERVAL_MS,
    );
  }

  /** Install document callbacks once the component instance is ready. */
  setAntiEntropy({ getVersion, makeDelta, makeSnapshot }) {
    this.antiEntropy = { getVersion, makeDelta, makeSnapshot };
    this._publishControl("hello", this._encodedVersion());
  }

  advertise() {
    if (!this.antiEntropy || this.closed) return;
    this._publishControl("version", this._encodedVersion());
  }

  gossip(bytes, { force = false } = {}) {
    const encoded = b64(bytes);
    const key = `artifact:${encoded}`;
    if (!force && !this.seenArtifacts.admit(key)) return false;
    if (force) this.seenArtifacts.admit(key);
    this._publish({ t: "bin", from: this.id, b64: encoded });
    return true;
  }

  close() {
    this.closed = true;
    clearInterval(this.antiEntropyTimer);
    clearTimeout(this.wsReconnectTimer);
    this.ch.close();
    this.ws?.close();
    for (const pc of this.peers.values()) pc.close();
    this.peers.clear();
  }

  async offer() {
    const pc = this._pc();
    const dc = pc.createDataChannel("esbt");
    this._dc(pc, dc);
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await iceDone(pc);
    return JSON.stringify(pc.localDescription);
  }

  async answer(offerJson) {
    const pc = this._pc();
    pc.ondatachannel = (event) => this._dc(pc, event.channel);
    await pc.setRemoteDescription(JSON.parse(offerJson));
    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);
    await iceDone(pc);
    return JSON.stringify(pc.localDescription);
  }

  async applyAnswer(answerJson) {
    const pc = [...this.peers.values()].find(
      (candidate) => candidate.signalingState === "have-local-offer",
    );
    if (!pc) throw new Error("no local offer waiting");
    await pc.setRemoteDescription(JSON.parse(answerJson));
  }

  _connectWs() {
    if (this.closed) return;
    const protocol = location.protocol === "https:" ? "wss" : "ws";
    const url = `${protocol}://${location.host}/signal?room=${encodeURIComponent(this.room)}`;
    let ws;
    try {
      ws = new WebSocket(url);
    } catch {
      this._scheduleReconnect();
      return;
    }
    this.ws = ws;
    ws.onmessage = (event) => {
      try {
        this._take(JSON.parse(event.data), "ws");
      } catch {
        // The demo relay is untrusted input. Invalid JSON is dropped.
      }
    };
    ws.onopen = () => {
      if (this.ws !== ws || this.closed) return;
      this.wsAttempt = 0;
      for (const queued of this.wsQueue.splice(0)) ws.send(queued);
      this.wsQueueChars = 0;
      this._publishControl("hello", this._encodedVersion());
      this.onPeer?.({ via: "ws", id: this.id, state: "open" });
    };
    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      this.onPeer?.({ via: "ws", id: this.id, state: "closed" });
      this._scheduleReconnect();
    };
    ws.onerror = () => ws.close();
  }

  _scheduleReconnect() {
    if (this.closed || this.wsReconnectTimer !== null) return;
    const exponential = Math.min(MAX_RECONNECT_MS, 250 * 2 ** this.wsAttempt);
    const delay = Math.round(exponential * (0.75 + Math.random() * 0.5));
    this.wsAttempt = Math.min(this.wsAttempt + 1, 16);
    this.wsReconnectTimer = setTimeout(() => {
      this.wsReconnectTimer = null;
      this._connectWs();
    }, delay);
  }

  _publishControl(type, version) {
    const message = {
      t: type,
      from: this.id,
      n: ++this.controlSequence,
      ...(version ? { version } : {}),
    };
    this.seenControl.admit(controlKey(message));
    this._publish(message);
  }

  _encodedVersion() {
    if (!this.antiEntropy) return undefined;
    try {
      return b64(this.antiEntropy.getVersion());
    } catch {
      return undefined;
    }
  }

  _take(message, via) {
    if (!message || typeof message !== "object" || message.from === this.id) return;
    if (message.t === "bin") {
      if (typeof message.b64 !== "string") return;
      const key = `artifact:${message.b64}`;
      if (!this.seenArtifacts.admit(key)) return;
      let bytes;
      try {
        bytes = unb64(message.b64);
      } catch {
        return;
      }
      let accepted = false;
      try {
        accepted = this.onBytes(bytes) !== false;
      } catch {
        accepted = false;
      }
      if (accepted) this._relay(message, via);
      return;
    }
    if (message.t !== "hello" && message.t !== "version") return;
    if (!Number.isSafeInteger(message.n) || message.n < 1) return;
    if (!this.seenControl.admit(controlKey(message))) return;
    this._relay(message, via);
    this.onPeer?.({ via, id: message.from, state: message.t === "hello" ? "joined" : "sync" });
    if (typeof message.version === "string") this._answerVersion(message.version);
    if (message.t === "hello" && this.antiEntropy) {
      this._publishControl("version", this._encodedVersion());
    }
  }

  _answerVersion(encoded) {
    if (!this.antiEntropy) return;
    let remoteVersion;
    try {
      remoteVersion = unb64(encoded);
    } catch {
      return;
    }
    try {
      this.gossip(this.antiEntropy.makeDelta(remoteVersion));
    } catch (error) {
      if (error?.code !== 21) return;
      try {
        this.gossip(this.antiEntropy.makeSnapshot());
      } catch {
        // A document that cannot make a causally closed recovery artifact
        // waits for the next anti-entropy round instead of sending bad state.
      }
    }
  }

  _publish(message) {
    this.ch.postMessage(message);
    this._rtcSend(message);
    this._wsSend(message);
  }

  _relay(message, via) {
    if (via !== "tab") this.ch.postMessage(message);
    if (via !== "webrtc") this._rtcSend(message);
    if (via !== "ws") this._wsSend(message);
  }

  _wsSend(message) {
    const encoded = JSON.stringify(message);
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(encoded);
      return;
    }
    this.wsQueue.push(encoded);
    this.wsQueueChars += encoded.length;
    while (
      this.wsQueue.length > MAX_WS_QUEUE_ENTRIES
      || this.wsQueueChars > MAX_WS_QUEUE_CHARS
    ) {
      this.wsQueueChars -= this.wsQueue.shift().length;
    }
  }

  _rtcSend(message) {
    const encoded = JSON.stringify(message);
    for (const pc of this.peers.values()) {
      if (pc._dc?.readyState === "open") {
        try {
          pc._dc.send(encoded);
        } catch {
          // A reconnect/version round repairs any missed data-channel send.
        }
      }
    }
  }

  _pc() {
    const pc = new RTCPeerConnection({ iceServers: STUN });
    pc._pid = crypto.randomUUID();
    this.peers.set(pc._pid, pc);
    pc.onconnectionstatechange = () => {
      this.onPeer?.({ via: "webrtc", id: pc._pid, state: pc.connectionState });
      if (["closed", "failed"].includes(pc.connectionState)) this.peers.delete(pc._pid);
    };
    return pc;
  }

  _dc(pc, dc) {
    pc._dc = dc;
    dc.onmessage = (event) => {
      try {
        this._take(JSON.parse(event.data), "webrtc");
      } catch {
        // Drop malformed peer frames.
      }
    };
    dc.onopen = () => {
      this.onPeer?.({ via: "webrtc", id: pc._pid, state: "open" });
      this._publishControl("hello", this._encodedVersion());
    };
  }
}

function controlKey(message) {
  return `control:${message.from}:${message.n}`;
}

function iceDone(pc) {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, 2_000);
    pc.onicegatheringstatechange = () => {
      if (pc.iceGatheringState === "complete") {
        clearTimeout(timer);
        resolve();
      }
    };
  });
}

function b64(bytes) {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 16_384) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 16_384));
  }
  return btoa(binary);
}

function unb64(encoded) {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
}
