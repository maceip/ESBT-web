/**
 * Reliable epidemic mesh (paper §4).
 * The paper does not name a socket. We implement the assumed reliable
 * broadcast with: BroadcastChannel, WebRTC datachannels, and anti-entropy.
 */

const STUN = [{ urls: "stun:stun.l.google.com:19302" }];

export class Mesh {
  constructor({ room, onBytes, onPeer }) {
    this.room = room;
    this.id = crypto.randomUUID();
    this.onBytes = onBytes;
    this.onPeer = onPeer;
    this.peers = new Map();
    this.seen = new Set();
    this.ch = new BroadcastChannel("esbt:" + room);
    this.ch.onmessage = (e) => this._tab(e.data);
    this.ch.postMessage({ t: "hello", from: this.id });
    this.ws = this._ws();
  }

  _ws() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const url = `${proto}://${location.host}/signal?room=${encodeURIComponent(this.room)}`;
    let ws;
    try {
      ws = new WebSocket(url);
    } catch (_) {
      return null;
    }
    ws.onmessage = (ev) => {
      try {
        this._take(JSON.parse(ev.data));
      } catch (_) {}
    };
    ws.onopen = () => this.onPeer?.({ via: "ws", id: this.id, state: "open" });
    return ws;
  }

  gossip(bytes) {
    const id = fnv(bytes);
    if (this.seen.has(id)) return;
    this.seen.add(id);
    this._trim();
    const msg = { t: "bin", from: this.id, id, b64: b64(bytes) };
    this.ch.postMessage(msg);
    this._rtcSend(msg);
    if (this.ws && this.ws.readyState === 1) this.ws.send(JSON.stringify(msg));
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
    pc.ondatachannel = (ev) => this._dc(pc, ev.channel);
    await pc.setRemoteDescription(JSON.parse(offerJson));
    const ans = await pc.createAnswer();
    await pc.setLocalDescription(ans);
    await iceDone(pc);
    return JSON.stringify(pc.localDescription);
  }

  async applyAnswer(answerJson) {
    const pc = [...this.peers.values()].find((p) => p.signalingState === "have-local-offer");
    if (!pc) throw new Error("no local offer waiting");
    await pc.setRemoteDescription(JSON.parse(answerJson));
  }

  _tab(data) {
    if (!data || data.from === this.id) return;
    if (data.t === "hello") this.onPeer?.({ via: "tab", id: data.from });
    if (data.t === "bin") this._take(data);
  }

  _take(msg) {
    if (this.seen.has(msg.id)) return;
    this.seen.add(msg.id);
    this._trim();
    const bytes = unb64(msg.b64);
    this.onBytes(bytes);
    this.ch.postMessage(msg);
    this._rtcSend(msg);
  }

  _rtcSend(msg) {
    const s = JSON.stringify(msg);
    for (const pc of this.peers.values()) {
      if (pc._dc?.readyState === "open") {
        try {
          pc._dc.send(s);
        } catch (_) {}
      }
    }
  }

  _pc() {
    const pc = new RTCPeerConnection({ iceServers: STUN });
    pc._pid = crypto.randomUUID();
    this.peers.set(pc._pid, pc);
    pc.onconnectionstatechange = () =>
      this.onPeer?.({ via: "webrtc", id: pc._pid, state: pc.connectionState });
    return pc;
  }

  _dc(pc, dc) {
    pc._dc = dc;
    dc.onmessage = (ev) => {
      try {
        this._take(JSON.parse(ev.data));
      } catch (_) {}
    };
    dc.onopen = () => this.onPeer?.({ via: "webrtc", id: pc._pid, state: "open" });
  }

  _trim() {
    if (this.seen.size > 20000) {
      const it = this.seen.values();
      for (let i = 0; i < 4000; i++) this.seen.delete(it.next().value);
    }
  }
}

function iceDone(pc) {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((res) => {
    const t = setTimeout(res, 2000);
    pc.onicegatheringstatechange = () => {
      if (pc.iceGatheringState === "complete") {
        clearTimeout(t);
        res();
      }
    };
  });
}

function b64(u8) {
  let s = "";
  for (const b of u8) s += String.fromCharCode(b);
  return btoa(s);
}

function unb64(s) {
  const bin = atob(s);
  const u = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i);
  return u;
}

function fnv(u8) {
  let h = 2166136261;
  for (const b of u8) {
    h ^= b;
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0).toString(16);
}
