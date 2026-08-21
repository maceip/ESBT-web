import { Esbt } from "./esbt.js";
import { Mesh } from "./mesh.js";

const $ = (id) => document.getElementById(id);

function roomId() {
  const u = new URL(location.href);
  let r = u.searchParams.get("room");
  if (!r) {
    r = crypto.randomUUID().slice(0, 8);
    u.searchParams.set("room", r);
    history.replaceState(null, "", u);
  }
  return r;
}

function siteId() {
  const key = "esbt-site:" + roomId();
  let s = Number(localStorage.getItem(key));
  if (!s || s < 1 || s > 63) {
    s = 1 + Math.floor(Math.random() * 62);
    localStorage.setItem(key, String(s));
  }
  return s;
}

const ROOM = roomId();
const SITE = siteId();
const docEl = $("doc");

let esbt;

const mesh = new Mesh({
  room: ROOM,
  onBytes(bytes) {
    if (!esbt) return;
    const tag = bytes[0];
    esbt.ingest(SITE, bytes);
    if (tag === 2) {
      for (const m of esbt.fillGap(SITE, bytes)) mesh.gossip(m);
      const snap = esbt.snapshot(SITE);
      if (snap) mesh.gossip(snap);
    }
    paint();
    persist();
  },
  onPeer(info) {
    $("status").textContent = `${info.via} ${info.state || "joined"}`;
    if (esbt) {
      const h = esbt.hello(SITE);
      if (h) mesh.gossip(h);
    }
  },
});

function paint() {
  const text = esbt.text(SITE);
  if (document.activeElement === docEl) {
    const sel = saveSel();
    if (docEl.innerText !== text) {
      docEl.innerText = text;
      restoreSel(sel);
    }
  } else if (docEl.innerText !== text) {
    docEl.innerText = text;
  }
  $("meta").innerHTML = `δ=${SITE} · n=${esbt.len(SITE)} · hash=${esbt
    .hash(SITE)
    .toString(16)} · Q=${esbt.pending(SITE)} · room=${ROOM}`;
  $("weights").textContent = JSON.stringify(esbt.weights(SITE), null, 2);
}

function saveSel() {
  const sel = getSelection();
  if (!sel || !sel.rangeCount) return { start: 0, end: 0 };
  const r = sel.getRangeAt(0);
  const pre = r.cloneRange();
  pre.selectNodeContents(docEl);
  pre.setEnd(r.startContainer, r.startOffset);
  const start = pre.toString().length;
  return { start, end: start + r.toString().length };
}

function restoreSel({ start, end }) {
  const walk = document.createTreeWalker(docEl, NodeFilter.SHOW_TEXT);
  let pos = 0;
  const range = document.createRange();
  let setS = false,
    setE = false;
  while (walk.nextNode()) {
    const n = walk.currentNode;
    const len = n.nodeValue.length;
    if (!setS && start <= pos + len) {
      range.setStart(n, Math.max(0, start - pos));
      setS = true;
    }
    if (!setE && end <= pos + len) {
      range.setEnd(n, Math.max(0, end - pos));
      setE = true;
      break;
    }
    pos += len;
  }
  if (setS) {
    const sel = getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  }
}

docEl.addEventListener("beforeinput", (ev) => {
  if (!esbt) return;
  ev.preventDefault();
  const { start, end } = saveSel();
  const span = Math.max(0, end - start);
  const out = [];
  if (span && (ev.inputType.startsWith("insert") || ev.inputType.startsWith("delete"))) {
    out.push(...esbt.deleteRange(SITE, start, span));
  } else if (ev.inputType.startsWith("delete") && span === 0) {
    const at = ev.inputType.includes("Backward") ? start - 1 : start;
    if (at >= 0) out.push(...esbt.deleteRange(SITE, at, 1));
  }
  if (ev.inputType.startsWith("insert") && ev.data) {
    const at = ev.inputType.startsWith("delete") ? start : Math.min(start, end);
    out.push(...esbt.insertUtf8(SITE, span ? start : at, ev.data));
  }
  if (ev.inputType === "insertFromPaste") {
    const data = ev.dataTransfer?.getData("text/plain") || ev.data || "";
    out.push(...esbt.insertUtf8(SITE, start, data));
  }
  for (const m of out) mesh.gossip(m);
  paint();
  persist();
});

async function persist() {
  try {
    const snap = esbt.snapshot(SITE);
    if (snap) localStorage.setItem("esbt-snap:" + ROOM, btoa(String.fromCharCode(...snap)));
  } catch (_) {}
}

function restoreLocal() {
  const raw = localStorage.getItem("esbt-snap:" + ROOM);
  if (!raw) return;
  try {
    const bin = Uint8Array.from(atob(raw), (c) => c.charCodeAt(0));
    esbt.ingest(SITE, bin);
  } catch (_) {}
}

$("copy").onclick = async () => {
  await navigator.clipboard.writeText(location.href);
  $("status").textContent = "room link copied";
};
$("offer").onclick = async () => {
  $("sdp").value = await mesh.offer();
};
$("accept").onclick = async () => {
  $("sdp").value = await mesh.answer($("sdp").value);
};
$("finish").onclick = async () => {
  await mesh.applyAnswer($("sdp").value);
};
$("verify").onclick = () => {
  const { rc, log } = esbt.verify();
  $("vlog").textContent = log;
  $("status").innerHTML = rc > 0 ? `<span class="ok">tests ${rc}</span>` : `<span class="bad">${rc}</span>`;
};

const es = await Esbt.load("./esbt.wasm");
es.init();
es.addReplica(SITE);
esbt = es;
restoreLocal();
paint();
const hello = esbt.hello(SITE);
if (hello) mesh.gossip(hello);
const snap = esbt.snapshot(SITE);
if (snap && esbt.len(SITE) > 0) mesh.gossip(snap);

window.__esbtDemo = {
  site: SITE,
  room: ROOM,
  text: () => esbt.text(SITE),
  hash: () => esbt.hash(SITE),
  len: () => esbt.len(SITE),
  pending: () => esbt.pending(SITE),
  weights: () => esbt.weights(SITE),
  verify: () => esbt.verify(),
};
