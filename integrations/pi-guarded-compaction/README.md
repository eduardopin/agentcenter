# Private Pi guarded-compaction runtime

Opt-in integration for `@earendil-works/pi-coding-agent` 0.87.0, Node 22.23.1
and Linux. It adds `compactGuarded` to a private SDK copy. It does not patch
the installed Pi, activate AgentCenter controls or change existing sessions.

## Prepare and verify

Run from this checkout's root. `SDK` must name the original, unmodified package
whose complete SDK and dependency inventories match `manifest.json`.

```sh
npm ci --prefix integrations/pi-guarded-compaction --ignore-scripts --no-audit --no-fund
SDK=/path/to/node_modules/@earendil-works/pi-coding-agent
mkdir -p target
node integrations/pi-guarded-compaction/prepare.mjs prepare "$SDK" "$PWD/target/pi-guarded-runtime"
node integrations/pi-guarded-compaction/prepare.mjs verify "$PWD/target/pi-guarded-runtime"
```

Preparation refuses existing output, mismatched inputs and redirected paths.
It copies dependencies, including internal relative symlinks, without linking
back to the installed SDK. Keep this checkout in place: the generated launcher
imports its verifier from here. Rebuild into a fresh path after changing the
transformer. Do not regenerate pins merely to bypass a mismatch.

The opt-in entrypoint is `node target/pi-guarded-runtime/run.mjs`, which verifies
the runtime before starting `dist/cli.js`. The installed CLI uses
`dist/bundle/cli.js`; this integration does not rebuild or modify that bundle.
Launching an interactive session is a separate operator action, not a test step.

## Offline checks

Tests expect the runtime at the path used above. They create synthetic sessions
and an offline provider; they never attach to an existing session.

```sh
PI_OFFLINE=1 npm test --prefix integrations/pi-guarded-compaction
node integrations/pi-guarded-compaction/node_modules/typescript/bin/tsc --noEmit --strict --skipLibCheck --module NodeNext --moduleResolution NodeNext --target ES2022 --allowImportingTsExtensions --lib ES2022,DOM integrations/pi-guarded-compaction/tests/api-types.mts
```

The typecheck covers the public consumer, not the entire upstream SDK.
See [the contract](../../docs/pi-guarded-compaction.md) for feature detection,
rejection codes, commit semantics and limits. Upstream source provenance and
MIT attribution are in `NOTICE` and `LICENSE.pi`.

## Reversal

Stop using the private entrypoint and use the unmodified installed Pi for future
sessions. The generated directory is disposable once no process uses it; no
uninstall or global repair is needed. Reversal does not undo a compaction that
has already committed to a session file.
