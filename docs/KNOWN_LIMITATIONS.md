# Known limitations

AgentCenter is pre-alpha. A successful build does not mean its acceptance,
performance or review gates have passed. Do not treat it as a stable release.

## Current validation

The integrated Rust workspace passed 412 tests in the latest development run,
with no failures and one pre-existing helper marked ignored; that helper is
invoked by its parent test. The Pi extension passed 22 tests separately.
These results apply to the tested development sources, not to a future release.

The full CI gate failed: its 500-process D-state fixture measured a median
5.036 ms against a budget below 5 ms. The current GUI validation also failed
its 300 ms first-frame and 2% CPU budgets: the integrated candidate measured
456.4 ms and 12.198% of one core. PSS was 50.1 MiB, within the 80 MiB budget.
The baseline also exceeded the first-frame budget. These observations do not
establish a regression's cause, and the budgets have not been relaxed.

## Session actions

Handoff and Compact are explicit, opt-in Pi controls. They require a compatible
extension and a privately prepared guarded Pi runtime; the ordinary Pi SDK
must not silently fall back to an unguarded control path. See the
[control contract](../extensions/pi-control/README.md) and
[guarded runtime setup](../integrations/pi-guarded-compaction/README.md).
No runtime is installed or attached to an existing session automatically.

Positive tests use temporary sessions and a deterministic fake provider. They
verify saved handoffs, native compaction entries and preserved session identity.
They are not proof of safe operation in an owner's live session. Some negative
GUI-to-socket scenarios, complete keyboard/accessibility coverage and small-screen
confirmation scrolling still lack acceptance evidence. Handoff sends bounded
projected session text to the provider selected in Pi, with explicit consent;
the monitor receives operation metadata, not the summary's content.

Independent approval of the latest control changes remains outstanding. Do not
interpret a published source snapshot, passing focused tests or a release-mode
binary as approval of those changes or completion of the gates above.
