# Pi control protocol 2

Explicit opt-in extension for the private Pi 0.87.0 runtime from CC0. The factory
registers handlers only; `session_start` opens resources and `session_shutdown`
revokes them. Nothing installs in HOME, attaches to an existing TUI or controls
an existing session during tests. The installed bundled Pi does not gain this API.

## Operator activation

Prepare and verify the private runtime using
[the CC0 instructions](../../integrations/pi-guarded-compaction/README.md).
An operator can later start a new private session with this extension:

```sh
node target/pi-guarded-runtime/run.mjs -e "$PWD/extensions/pi-control/index.ts"
```

That command starts a session; it is not an installation or test command. Keep
`XDG_RUNTIME_DIR` private (0700) and configure the desired authenticated model in
that Pi profile. Handoff may send up to 48 KiB of projected conversation text to
the selected provider and generate up to 2048 output tokens. The GUI must obtain
consent for the target, revision, effect and possible token cost before dispatch.
It receives metadata only, never the generated content. Compact uses native
`compactGuarded`, not a prompt, keystroke, `ctx.abort` or legacy fallback.

## Wire contract

One JSON object followed by LF per Unix connection; no arbitrary text, shell or
output path supplied by a client. Requests have exactly these fields:

```json
{"protocol_version":2,"operation_id":"UUID","action":"compact","session_id":"session","session_file":"/absolute/session.jsonl","pid":123,"process_starttime":"12345","instance_nonce":"64 lowercase hex characters","expected":{"managerId":"manager UUID","revision":"12","sessionId":"session","sessionFile":"/absolute/session.jsonl","leafId":null}}
```

Actions are `handoff`, `compact`, `status` and `cancel`. Preserve the complete
confirmed request for status/cancel: a changed leaf does not authorize a silent
refresh. Same ID and same identity/action replay the previous operation without
another effect. Each instance retains at most 128 tombstones; after 30 minutes
an existing ID returns `unknown/expired`, never restarts. Timeout after sending
means unknown; query the same ID instead of issuing another write.

Discovery manifests under `$XDG_RUNTIME_DIR/ac-*/manifest.json` contain the
protocol, native/process identity, nonce, socket, expected snapshot,
`handoff_directory`, `model_auth` and `capabilities`. Handoff and compact each
have `enabled`, `disabled_reason` and `cancel_supported`. Exact SDK capability
`agentcenter.compactGuarded.v1` and an authenticated current model are required.
Busy/pending, missing extension/model/auth and incompatible versions disable
execution with an explicit reason. Nonce is a local secret and is redacted from
Rust Debug output.

Replies have `protocol_version`, `operation_id`, `state` and a closed `code`.
States are accepted, running, completed, rejected, failed, cancelled and unknown.
Accepted/running never mean success. Authenticated replies add live metadata;
early transport rejection can omit it. Completed replies include `completed_at`:
Handoff adds `artifact_path` and the original expected snapshot; Compact adds
`compaction:{capability,sessionId,sessionFile,previousLeafId,entryId}`. The latter
identifies a persisted native entry, not a dispatched callback. Metadata can
already contain the new revision: retain the original request to reconcile it.

Handoff files are private 0600 regular files under the fixed
`$XDG_STATE_HOME/agentcenter/handoffs` (default `~/.local/state/agentcenter/handoffs`).
Names include the exact UTF-8 hex session ID, a random nonce and operation UUID;
overlong names reject before the model call. The header records identity,
expected revision, operation and bounded read-only git ground truth or an
explicit unavailable result. Publication uses atomic no-replace linking and
fsync. The monitor discovers only filename/age by exact session identity; it
never opens the Markdown. Handoff does not clear, compact, close, fork, create,
switch or resume a session.

## Trust and timing

The socket is 0600 beneath 0700 directories. Node relies on kernel DAC plus
nonce, not a nonexistent Node peercred getter. Rust validates directory/file
ownership, symlinks, manifest/socket identity, `/proc` starttime and Linux
SO_PEERCRED before sending. Same-UID processes and root are outside this trust
boundary. Four connections, 16 KiB frames and a two-second idle limit bound the
receiver; Rust uses one monotonic deadline, at most five seconds, with
nonblocking connect/write/read. Model generation has its own 60-second signal.
Cancellation is cooperative and cannot undo an already persisted result.

Herdr focus is separate: explicit workspace/tab/pane IDs, matching adapter,
process starttime and revalidated mapping. It verifies `pane_focused`, never
uses `agent.prompt`, current/default targeting or claims GNOME window activation.
Herdr does not offer server-side process CAS for focus; that limit is retained.

## Offline checks

```sh
# Original SDK is a read-only negative control; private SDK must already exist.
PI_SDK_DIR=/path/to/original/pi-coding-agent \
AGENTCENTER_GUARDED_PI_SDK="$PWD/target/pi-guarded-runtime" \
PI_OFFLINE=1 node --test extensions/pi-control/*.test.mjs
cargo test -p agentcenter-core control:: --lib --offline
cargo test -p agentcenter-core --test handoff_agentcenter --offline
```

Both SDK paths are required inputs; there are no developer-specific path
defaults. Missing inputs or a missing private SDK fail, without skips or fallback. The cross-language
fixture builds `examples/pi_control_probe.rs` offline and exercises Rust
 discovery, socket dispatch and same-ID polling against the actual Node
extension operations and private SDK with a fake provider. It checks both the
Handoff file and native CompactionEntry. Global/original SDK negative coverage
and the old no-CAS control remain separate. Herdr tests use an isolated fake
server, not the owner's panes. GUI integration, visual QA and live UAT are
separate acceptance steps; this README does not authorize them.
