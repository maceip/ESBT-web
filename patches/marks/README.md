# marks integration: remove Loro and Yjs, run on ESBT

A `git am`-ready series against [maceip/marks](https://github.com/maceip/marks)
`main` (`2ad6571`, "Land Cloud Agent browser harness from closed env-setup
PRs"). It ships here because this repository's automation has no push access
to marks; the series was developed and tested against a local marks checkout.

## Apply

```bash
git clone https://github.com/maceip/marks && cd marks
git checkout -b esbt-integration
git am path/to/ESBT-web/patches/marks/*.patch
npm install            # regenerates node_modules; the lockfile is in the series
npm run typecheck
npm run test:esbt      # 36 engine contract tests
npm run build
npm start &            # then, against the running build:
npm run smoke          # 37 end-to-end checks
```

## What each patch does

1. **Complete `@marks/esbt`** — the TypeScript ESBT engine (same sources as
   [`../../ts/`](../../ts/), which is the canonical copy), replacing the
   partial fragments that referenced modules which did not exist. The package
   builds to `dist/` so Vite and Node both consume plain ESM, and carries the
   36-test contract suite.
2. **Server** — `EsbtRoom` (full-replica room, version-vector delta on join,
   debounced persist, idle eviction, discard-on-delete) with a stable
   per-document server site id (`marks-server:<id>`, contract §6). Only
   `/collab/esbt/:id` upgrades; retired paths and legacy-engine rows are
   refused. Hocuspocus, `yjs`, and `loro-crdt` leave `server/package.json`.
3. **Client** — `EsbtEngine` (IndexedDB warm open, shallow HTTP snapshot,
   reconnect with `?vv=`, 500 ms undo grouping) plus
   `collab/presence.ts`, the CodeMirror decoration layer replacing
   `loro-codemirror` / `y-codemirror.next`: publishes the contract's
   `${siteId}-cm-user` / `${siteId}-cm-sel` keys on a 15 s heartbeat and
   draws remote carets and selections. Single-engine UI, legacy rows are
   listed but refused with an explanation, benchmark page measures ESBT.
   Six packages leave `client/package.json`.
4. **Docs, smoke, harness** — README for the single-engine reality with
   honest numbers, a Status section in `docs/ESBT-INTEGRATION.md` recording
   the three contract additions the coverage audit forced
   (`EphemeralStore.keys()`, `UndoManagerOptions.mergeIntervalMs`, weight
   anchors), and the smoke suite's Yjs section becoming a second-document
   section with the remote-cursor probe pointed at `.esbt-caret`.
5. **Comment sweep** — protocol headers, editor notes, and the unused
   `TEXT_KEY` export.

## Test results on the assembled tree

Run on the exact tree these patches produce (Node 22.14, Chrome, Linux):

```
npm run typecheck      esbt + client + server: clean
npm run test:esbt      36/36 contract tests pass
npm run build          esbt (tsc), client (vite), server (tsc): clean
npm run smoke          37/37 end-to-end checks pass
benchmark page         quick trace: converged, no console errors
```

Smoke coverage that specifically exercises the replacement: two-peer
convergence, presence avatars, remote caret drawn (`.esbt-caret`), per-user
undo that spares the collaborator's text, checkbox write-back from the
preview, offline editing with delta resync on reconnect, second-document cold
open by a second browser, deletion tombstone while a room is live, and
refusal of the retired `/collab/loro/:id` socket path.
