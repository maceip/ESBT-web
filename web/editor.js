import { EsbtDocument } from "./esbt-document.js";
import { Mesh } from "./mesh.js";

const $ = (id) => document.getElementById(id);
const textDecoder = new TextDecoder();

function roomId() {
  const url = new URL(location.href);
  let room = url.searchParams.get("room");
  if (!room) {
    room = crypto.randomUUID().slice(0, 8);
    url.searchParams.set("room", room);
    history.replaceState(null, "", url);
  }
  return room;
}

function siteId() {
  // A site identifies one live operation generator, not a room or product
  // device. Reusing it in two tabs would let both mint the same counters.
  // Restored state carries its old receipts, so a reload can safely join with
  // a fresh generator identity.
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  if (bytes.every((byte) => byte === 0)) bytes[0] = 1;
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

const ROOM = roomId();
const SITE = siteId();
const docEl = $("doc");
const earlyMessages = [];
const ingestErrors = [];
let documentCore = null;
let lastIngest = null;
let typingGroup = 0n;
let lastTypingAt = 0;

const mesh = new Mesh({
  room: ROOM,
  onBytes(bytes) {
    if (!documentCore) {
      earlyMessages.push(bytes.slice());
      return;
    }
    ingest(bytes);
  },
  onPeer(info) {
    $("status").textContent = `${info.via} ${info.state || "joined"}`;
    if (!documentCore) return;
    try {
      mesh.gossip(documentCore.exportFullSnapshot(), { force: true });
    } catch (error) {
      recordError(-1, error);
    }
  },
});

function ingest(bytes) {
  const selection = selectionAnchors();
  const tag = bytes.length >= 7 ? bytes[6] : -1;
  try {
    const receipt = documentCore.import(bytes);
    lastIngest = {
      tag,
      result: receipt?.outcome || receipt?.kind || "ok",
      bytes: bytes.length,
    };
    // A newly opened peer advertises its full archive even when empty. Every
    // established peer answers that advertisement with its own archive. Mesh
    // content de-duplication makes this a finite anti-entropy exchange.
    if (tag === 6) mesh.gossip(documentCore.exportFullSnapshot());
  } catch (error) {
    recordError(tag, error, bytes.length);
  }
  paint(selection);
  persist();
}

function recordError(tag, error, bytes = 0) {
  lastIngest = { tag, result: error.code ?? -1, message: String(error.message || error), bytes };
  ingestErrors.push(lastIngest);
  if (ingestErrors.length > 16) ingestErrors.shift();
}

function selectionOffsets() {
  const selection = getSelection();
  if (!selection || !selection.rangeCount) return { start: 0, end: 0 };
  const range = selection.getRangeAt(0);
  if (!docEl.contains(range.startContainer) || !docEl.contains(range.endContainer)) {
    return { start: 0, end: 0 };
  }
  const before = range.cloneRange();
  before.selectNodeContents(docEl);
  before.setEnd(range.startContainer, range.startOffset);
  const start = before.toString().length;
  return { start, end: start + range.toString().length };
}

function selectionAnchors() {
  if (!documentCore || document.activeElement !== docEl) return null;
  const { start, end } = selectionOffsets();
  try {
    return {
      start: documentCore.indexToAnchor(start, "after"),
      end:
        start === end
          ? documentCore.indexToAnchor(end, "after")
          : documentCore.indexToAnchor(end, "before"),
    };
  } catch (_) {
    return null;
  }
}

function paint(anchoredSelection = null) {
  if (!documentCore) return;
  const selection = anchoredSelection || selectionAnchors();
  const text = documentCore.getText();
  if (docEl.innerText !== text) docEl.innerText = text;
  if (selection) {
    const start = documentCore.anchorToIndex(selection.start);
    const end = documentCore.anchorToIndex(selection.end);
    restoreSelection(start, end);
  }
  $("meta").textContent = `δ=${SITE.slice(0, 8)}… · n=${documentCore.length} · hash=${documentCore
    .stateHash()
    .toString(16)} · Q=${documentCore.pendingOperations} · room=${ROOM}`;
  $("engine").textContent = JSON.stringify(
    {
      siteId: SITE,
      versionBytes: documentCore.version().length,
    },
    null,
    2,
  );
}

function restoreSelection(start, end) {
  const range = document.createRange();
  const walker = document.createTreeWalker(docEl, NodeFilter.SHOW_TEXT);
  let position = 0;
  let startSet = false;
  let endSet = false;
  while (walker.nextNode()) {
    const node = walker.currentNode;
    const length = node.nodeValue.length;
    if (!startSet && start <= position + length) {
      range.setStart(node, Math.max(0, start - position));
      startSet = true;
    }
    if (!endSet && end <= position + length) {
      range.setEnd(node, Math.max(0, end - position));
      endSet = true;
      break;
    }
    position += length;
  }
  if (!startSet) range.setStart(docEl, docEl.childNodes.length);
  if (!endSet) range.setEnd(docEl, docEl.childNodes.length);
  const selection = getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
}

function undoGroupFor(inputType) {
  const typing =
    inputType.startsWith("insertText") ||
    inputType === "insertCompositionText" ||
    inputType === "deleteContentBackward" ||
    inputType === "deleteContentForward";
  const now = performance.now();
  if (!typing || now - lastTypingAt > 750) typingGroup += 1n;
  lastTypingAt = now;
  return typingGroup;
}

docEl.addEventListener("beforeinput", (event) => {
  if (!documentCore) return;
  event.preventDefault();
  if (event.inputType === "historyUndo") {
    documentCore.undo({ origin: "undo" });
    paint();
    persist();
    return;
  }
  if (event.inputType === "historyRedo") {
    documentCore.redo({ origin: "redo" });
    paint();
    persist();
    return;
  }

  let { start, end } = selectionOffsets();
  let inserted = "";
  if (event.inputType === "insertParagraph" || event.inputType === "insertLineBreak") {
    inserted = "\n";
  } else if (event.inputType === "insertFromPaste") {
    inserted = event.dataTransfer?.getData("text/plain") || event.data || "";
  } else if (event.inputType.startsWith("insert")) {
    inserted = event.data || "";
  } else if (start === end && event.inputType.includes("Backward") && start > 0) {
    start -= 1;
  } else if (start === end && event.inputType.includes("Forward")) {
    end = Math.min(documentCore.length, end + 1);
  }

  try {
    documentCore.replaceRange(start, end, inserted, {
      origin: "editor",
      undoGroup: undoGroupFor(event.inputType),
    });
    const caret = documentCore.indexToAnchor(start + inserted.length, "after");
    paint({ start: caret, end: caret });
    persist();
  } catch (error) {
    recordError(-1, error);
    $("status").textContent = error.message;
  }
});

function persist() {
  if (!documentCore) return;
  try {
    localStorage.setItem(`esbt-snap:${ROOM}`, bytesToBase64(documentCore.exportFullSnapshot()));
  } catch (_) {}
}

function restoreLocal() {
  const encoded = localStorage.getItem(`esbt-snap:${ROOM}`);
  if (!encoded) return;
  try {
    documentCore.applySnapshot(base64ToBytes(encoded));
  } catch (error) {
    recordError(6, error);
  }
}

function bytesToBase64(bytes) {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 16_384) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 16_384));
  }
  return btoa(binary);
}

function base64ToBytes(encoded) {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
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

documentCore = await EsbtDocument.create({ siteId: SITE, wasmUrl: "./esbt.wasm" });
documentCore.onLocalUpdate((update) => {
  mesh.gossip(update);
  persist();
});
restoreLocal();
for (const bytes of earlyMessages.splice(0)) ingest(bytes);
paint();
// This is both our initial state advertisement and, when empty, a request for
// an established peer's full archive.
mesh.gossip(documentCore.exportFullSnapshot());

window.__esbtDemo = {
  site: SITE,
  room: ROOM,
  text: () => documentCore.getText(),
  hash: () => documentCore.stateHash(),
  len: () => documentCore.length,
  pending: () => documentCore.pendingOperations,
  diagnostics: () => ({ lastIngest, ingestErrors: [...ingestErrors] }),
};
