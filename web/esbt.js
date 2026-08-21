/** WASM bindings. Every mutation returns epidemic Message bytes. */

export class Esbt {
  constructor(exp) {
    this.w = exp;
  }

  static async load(url = "./esbt.wasm") {
    const res = await fetch(url);
    const buf = await res.arrayBuffer();
    const { instance } = await WebAssembly.instantiate(buf, { env: {} });
    return new Esbt(instance.exports);
  }

  u8() {
    return new Uint8Array(this.w.memory.buffer);
  }

  last() {
    const n = this.w.esbt_last_len();
    const p = this.w.esbt_last_ptr();
    return this.u8().slice(p, p + n);
  }

  lastText() {
    return new TextDecoder().decode(this.last());
  }

  init(dmax = 65536, base = 2147483647, depth = 256) {
    this.w.esbt_init(dmax | 0, base >>> 0, depth >>> 0);
  }

  addReplica(site) {
    return this.w.esbt_add_replica(site >>> 0);
  }

  len(site) {
    return this.w.esbt_len(site >>> 0);
  }

  text(site) {
    if (this.w.esbt_text(site >>> 0) < 0) return "";
    return this.lastText();
  }

  hash(site) {
    return this.w.esbt_hash(site >>> 0) >>> 0;
  }

  pending(site) {
    return this.w.esbt_pending(site >>> 0);
  }

  insert(site, index, ch) {
    const cp = typeof ch === "string" ? ch.codePointAt(0) : ch;
    if (this.w.esbt_insert(site >>> 0, index | 0, cp >>> 0) <= 0) return null;
    return this.last();
  }

  insertUtf8(site, index, str) {
    const bytes = new TextEncoder().encode(str);
    const p = this.w.esbt_malloc(bytes.length);
    this.u8().set(bytes, p);
    const rc = this.w.esbt_insert_utf8(site >>> 0, index | 0, p, bytes.length);
    this.w.esbt_free(p, bytes.length);
    if (rc < 0) return [];
    return unpackMsgs(this.last());
  }

  deleteRange(site, index, n) {
    if (this.w.esbt_delete_range(site >>> 0, index | 0, n | 0) < 0) return [];
    return unpackMsgs(this.last());
  }

  ingest(site, bytes) {
    const p = this.w.esbt_malloc(bytes.length);
    this.u8().set(bytes, p);
    const rc = this.w.esbt_ingest(site >>> 0, p, bytes.length);
    this.w.esbt_free(p, bytes.length);
    return rc;
  }

  snapshot(site) {
    if (this.w.esbt_snapshot(site >>> 0) < 0) return null;
    return this.last();
  }

  hello(site) {
    if (this.w.esbt_hello(site >>> 0) < 0) return null;
    return this.last();
  }

  fillGap(site, helloBytes) {
    const p = this.w.esbt_malloc(helloBytes.length);
    this.u8().set(helloBytes, p);
    const rc = this.w.esbt_fill_gap(site >>> 0, p, helloBytes.length);
    this.w.esbt_free(p, helloBytes.length);
    if (rc < 0) return [];
    return unpackMsgs(this.last());
  }

  weights(site) {
    if (this.w.esbt_weights_json(site >>> 0) < 0) return [];
    return JSON.parse(this.lastText());
  }

  verify() {
    const rc = this.w.esbt_verify();
    return { rc, log: this.lastText() };
  }
}

export function unpackMsgs(buf) {
  if (!buf || buf.length < 4) return [];
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const n = view.getUint32(0, true);
  const out = [];
  let i = 4;
  for (let k = 0; k < n && i + 4 <= buf.length; k++) {
    const ln = view.getUint32(i, true);
    i += 4;
    out.push(buf.slice(i, i + ln));
    i += ln;
  }
  return out;
}
