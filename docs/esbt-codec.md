# ESBT component and artifact contract

This is the complete interoperability contract for ESBT 0.3. There are two
layers, each with one job:

| Concern | WIT component contract | ESBT artifact codec |
|---|---|---|
| Scope | Calls between a host and one engine instance | Durable or transmitted CRDT state |
| Representation | Typed records, variants, resources, `result`, `list<u16>`, `u64` | Canonical byte strings |
| Carries | Configuration, visible edits, receipts, errors, document methods | Update, compact snapshot, full snapshot, version, anchor, causal position |
| Evolution boundary | `esbt:document@1.0.0` | `ESBT` envelope format field |
| Browser path | Jco-generated JavaScript plus core Wasm modules | Opaque `Uint8Array` values lifted through WIT |
| Debug path | Generated TypeScript declarations | `esbt-inspect` plus checked-in golden vectors |

Configuration, errors, receipts, visible edits, and operation references are
not separately serialized. They cross the host boundary as WIT values. This
is why ESBT does not also need Protobuf, Cap'n Proto, Cap'n Web, or a second
hand-written host ABI.

The source of truth for the host API is [`wit/esbt.wit`](../wit/esbt.wit).
The source of truth for artifact dispatch is [`src/wire.rs`](../src/wire.rs).

## Clean-break rule

Only the envelope in this document is accepted. There is no legacy decoder,
negotiation mode, or compatibility shim for the retired raw `esbt_doc_*` ABI
or the retired `ESBM`, `ESBS`, `ESBF`, and `ESBA` envelopes. Those byte
strings fail with `MalformedEncoding`.

The current envelope format field is `1` because this is the first format of
the unified `ESBT` codec. It does not identify or preserve any retired v1
format. During the current pre-product phase, a future incompatible codec
change replaces this format and its stored artifacts rather than silently
dual-reading old state, unless a migration is explicitly designed at that
time.

## Outer envelope

Every artifact has exactly one outer envelope. Nested values use their
payload encodings without another magic or version.

| Offset | Width | Encoding | Meaning |
|---:|---:|---|---|
| 0 | 4 | ASCII | `ESBT` |
| 4 | 2 | little-endian `u16` | format, currently `1` |
| 6 | 1 | `u8` | artifact kind |
| 7 | 4 | little-endian `u32` | payload byte length |
| 11 | declared length | kind-specific | canonical payload |

The declared length must consume the rest of the input exactly. Truncation,
trailing bytes, an unknown kind, or a different format fails closed.

| Kind | Value | Payload |
|---|---:|---|
| Update | 1 | Retry-safe operation batch |
| Compact snapshot | 2 | Materialized state and receipt frontiers |
| Full snapshot | 3 | Compact state plus reconnect/crash history |
| Version | 4 | Gap-aware operation receipts |
| Anchor | 5 | Stable document boundary |
| Causal position | 6 | Version frontier plus anchor |

## Primitive canonical encodings

- Fixed integers are little-endian. A site ID is a nonzero little-endian
  `u128` on the wire and a WIT `{ low: u64, high: u64 }` record at the host
  boundary.
- `varuint` is unsigned LEB128 limited to 64 bits. Non-minimal encodings,
  overflow, and unterminated encodings are rejected.
- Signed variable integers use ZigZag followed by canonical `varuint`.
- A UTF-16 unit is a little-endian `u16`. WIT lifts a sequence of them as
  `list<u16>`/`Uint16Array`; no UTF-8 round trip can change lone surrogates.
- Tables are sorted, strictly unique, and must contain exactly the entries
  referenced by their container. Operations and pending identities are also
  required to be in their declared canonical order.
- A rational is positive, finite, and reduced. Its numerator and denominator
  are each `varuint` values no larger than `i64::MAX`.

### Weight body

A document weight contains fraction `(p, q)`, signed sequence number `sn`,
sequence path `sc`, and owner site. It begins with a flags byte:

| Bit | Name | Meaning when set |
|---:|---|---|
| 0 | `sn-present` | A ZigZag `sn` follows `p` and `q`; encoded zero is forbidden |
| 1 | `sc-present` | An explicit path follows; absent means the canonical default `[0]` |
| 2 | `site-inline` | A 16-byte site follows in a self-contained/origin context |

All other bits are forbidden. In a site-table context bit 2 is forbidden and
a `varuint` table index always follows. In an operation-origin context the
site is omitted only when it equals the origin; an explicitly repeated origin
would be noncanonical.

An explicit self-contained path is `length: varuint` followed by that many
`u32 varuint` digits. A sorted container instead writes
`shared-prefix: varuint`, `suffix-length: varuint`, then the suffix digits.
The shared prefix must equal the actual longest common prefix with the
previous path. The decoder recomputes it, preventing multiple byte strings
for one identifier.

The fraction components are arithmetically bounded by the allocator's hard
`Dmax` ceiling. A complete identifier can still acquire sequence-path
components under a pathological repeated pinch. That growth is bounded by
the document's `max-identifier-depth` resource policy and fails atomically as
typed `IdentifierTooDeep`; ESBT does not claim a constant-size full
identifier under every adversarial history.

## Kind payloads

All `length` fields in this section are little-endian `u32` unless called a
`varuint`.

### 1. Update

```text
site-count: varuint
site[site-count]: u128
operation-count: varuint
operation[operation-count]:
  tag: u8                         # 1 insert, 2 delete
  origin-site-index: varuint
  origin-sequence: varuint        # nonzero
  insertion-counter: varuint      # nonzero target identity
  weight: front-coded weight body using the site table
  inserted-unit: u16              # insert only
```

Operations are strictly sorted by `(origin site, origin sequence)`. Sequence
paths are front-coded across that order. Insertion origin must equal the
weight owner. Every native `Update::new` and every wire decoder uses the same
operation/weight invariant validation.

### 2. Compact snapshot

```text
insertion-version-length
insertion-version payload
site-count: varuint
site[site-count]: u128
atom-count: varuint
atom[atom-count]:
  weight: front-coded weight body using the site table
  unit: u16
  insertion-counter: varuint
deferred-delete-count: varuint
deferred-delete[deferred-delete-count]:
  weight: front-coded weight body using the site table
  insertion-counter: varuint
version-length
version payload
```

Atoms and deferred deletes are strictly ordered by weight, their identities
must be coherent, and their two version frontiers must cover the represented
state without illegal gaps.

### 3. Full snapshot

```text
compact-state-length
compact-snapshot payload           # no nested ESBT envelope
history-floor-length
version payload
retained-update-length
update payload                     # no nested ESBT envelope
pending-count: u32
pending[pending-count]:
  origin: u128
  sequence: u64
```

The history floor is a contiguous prefix covered by the state. Retained and
pending operations must exactly explain state above that floor; pending
identities are strictly sorted and must name retained operations. This is the
crash-recovery artifact.

### 4. Version

```text
site-count: u32
site[site-count]:
  site-id: u128
  contiguous-prefix: u64
  sparse-count: u32
  sparse-sequence[sparse-count]: u64
```

Sites and sparse sequences are strictly ascending. A sparse value cannot be
zero, covered by the prefix, or exactly `prefix + 1` (that value must fold
into the prefix). Empty site entries are forbidden. This representation can
request exact reconnect holes rather than only a maximum counter.

### 5. Anchor

```text
affinity: u8                       # 1 before, 2 after
target: u8                         # 1 start, 2 end, 3 item
if target == item:
  self-contained weight body
  insertion-counter: u64
```

Start is canonically `after`; end is canonically `before`. An item anchor
names both the weight and insertion counter so later weight reuse cannot be
mistaken for the deleted item.

### 6. Causal position

```text
version-length
version payload
anchor-length
anchor payload
```

The version says when the anchor may be resolved. Before a replica covers
that frontier, WIT returns `none`; after coverage, normal deterministic
anchor-collapse rules apply.

## Resource and mutation guarantees

Every receiving `Document` decodes under its configured `resource-limits`.
Counts are checked against both those limits and the bytes remaining before
capacity is allocated. Arithmetic uses checked offsets and lengths. Decode
errors do not partially mutate a document: updates, snapshots, and local
transactions are staged and committed only after validation succeeds.

The allocation fraction ceiling is `DMAX_HARD_CEILING = 2^31`. Both static
and adaptive configuration above that ceiling are rejected at document
creation. The low-level allocator clamps direct construction defensively;
the public document/configuration surface reports the invalid policy instead
of silently accepting it.

## Browser execution

Safari, Firefox, and Chromium do not need to understand WIT or execute a
component binary natively. Jco transpiles the component into a generated
JavaScript binding and ordinary core Wasm modules. Browsers execute those
core modules through the standard WebAssembly API; the generated binding
performs canonical ABI lifting/lowering. Release verification checks:

- the component binary header and actual versioned WIT export;
- generated `Uint8Array`, `Uint16Array`, `bigint`, resource, and receipt
  declarations rather than only export names;
- every generated core module header and parse result;
- exact hashes and byte lengths in the Marks component manifest.

## Inspection and golden vectors

Build the inspector and validate an artifact semantically:

```bash
cargo run --bin esbt-inspect -- path/to/artifact.esbt
```

Use `--structural` to inspect only the envelope, or `--max-bytes N` to choose
an explicit input ceiling. Output is one JSON object suitable for logs and
bug reports. The command never resolves anchors or reveals document text; it
reports bounded structural counts and typed decode failures.

The six checked-in vectors live in
[`tests/golden/esbt-codec.txt`](../tests/golden/esbt-codec.txt). Regenerate
candidate output with:

```bash
cargo run --example generate_golden
```

`tests/golden_vectors.rs` requires every vector to decode and re-encode byte
exactly and proves all four retired split-envelope magics are rejected.
