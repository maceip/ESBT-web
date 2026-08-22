# marks integration: remove Loro and Yjs, run on ESBT

A `git am`-ready series against [maceip/marks](https://github.com/maceip/marks)
`main` at `c53c617` ("Defer comment highlight paints so CodeMirror is not
updated mid-update" — the head that includes the browser-surface feature:
comments, cross-tab sync, clipboard, context menu, voice, offline shell).

The same six commits are also pushed to marks directly as the
[`macprime-esbt-engine-3ead`](https://github.com/maceip/marks/tree/macprime-esbt-engine-3ead)
branch (head `04e4cb8`), so opening a PR there needs no patch application at
all. This directory remains the reviewable, self-contained copy of the series
and the record of how it was verified.

## Apply

```bash
git clone https://github.com/maceip/marks && cd marks
git checkout -b esbt-integration
git am path/to/ESBT-web/patches/marks/*.patch
npm install              # regenerates node_modules; the lockfile is in the series
npm run typecheck        # builds the engine first, then checks all workspaces
npm run test:esbt        # 40 engine contract tests
npm run test:browser     # 24 browser-surface unit tests
npm run build
npm start &              # then, against the running build:
npm run smoke            # 43 end-to-end checks
npm run smoke:surface    # 9 portable glass checks (Playwright driver)
```

## What each patch does

1. **Complete `@marks/esbt`** — the TypeScript ESBT engine (same sources as
   [`../../ts/`](../../ts/), the canonical copy), replacing the partial
   fragments that referenced modules which did not exist. Includes the keyed
   LWW map that carries comment records and the weight anchors that stand in
   for Loro Cursors. Builds to `dist/` so Vite and Node both consume plain
   ESM; ships the 40-test contract suite.
2. **Server** — `EsbtRoom` (full-replica room, version-vector delta on join,
   debounced persist, idle eviction, discard-on-delete) with a stable
   per-document server site id (`marks-server:<id>`, contract §6). Only
   `/collab/esbt/:id` upgrades; retired paths and legacy-engine rows are
   refused. Hocuspocus, `yjs`, and `loro-crdt` leave `server/package.json`.
3. **Client** — `EsbtEngine` with full browser-surface parity: IndexedDB warm
   open behind the persist lock, network-aware shallow HTTP snapshot,
   reconnect with `?vv=`, cross-tab `TabChannel` sync, hydration state,
   comments in the engine's LWW map with anchor-encoded cursors (the shared
   quote resolver stays as fallback), 500 ms undo grouping with comment
   origins excluded. `collab/presence.ts` replaces `loro-codemirror` /
   `y-codemirror.next`: publishes the contract's `${siteId}-cm-user` /
   `${siteId}-cm-sel` keys on a 15 s heartbeat and draws remote carets and
   selections as decorations. Single-engine UI; legacy rows listed but
   refused with an explanation; benchmark page measures ESBT. Six packages
   leave `client/package.json`.
4. **Docs, smoke, harness** — README for the single-engine reality with
   honest numbers, a Status section in `docs/ESBT-INTEGRATION.md` recording
   the four contract additions the coverage audit forced
   (`EphemeralStore.keys()`, undo merge window + origin exclusion, weight
   anchors, the keyed LWW map), and the smoke suite's Yjs section becoming a
   second-document section with the remote-cursor probe pointed at
   `.esbt-caret`.
5. **Comment sweep** — protocol headers, comment-cursor docs, harness helper
   names, editor notes.
6. **Typecheck ordering** — `npm run typecheck` builds the engine workspace
   first, since dependents resolve its types from `dist/`.

## Test results on the assembled tree

Run on a fresh `git clone` + `git am` of exactly these patches
(Node 22.14, Chrome, Linux):

```
npm run typecheck        esbt + client + server: clean
npm run test:esbt        40/40 contract tests pass
npm run test:browser     24/24 pass
npm run test:harness     7/7 pass
npm run build            esbt (tsc), client (vite), server (tsc): clean
npm run smoke            43/43 end-to-end checks pass
npm run smoke:surface    9/9 pass (Playwright driver)
```

Smoke coverage that specifically exercises the replacement: two-peer
convergence, presence avatars, remote caret drawn (`.esbt-caret`), per-user
undo that spares the collaborator's text, checkbox write-back from the
preview, **a comment stored on the document** (through the ESBT map with
weight anchors), offline editing with delta resync on reconnect,
second-document cold open by a second browser, deletion tombstone while a
room is live, and refusal of the retired `/collab/loro/:id` socket path.
