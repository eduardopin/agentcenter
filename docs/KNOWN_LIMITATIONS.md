# Known limitations

AgentCenter is pre-alpha. A successful build does not mean its acceptance,
performance or review gates have passed. Do not treat it as a stable release.

## Current validation

The Rust workspace passed 919 tests with no failures and 9 ignored in CI on
commit 014a336 (GitHub Actions run 42175066068, 6 November 2026). CI does not
run the Pi extension's tests; its last local run recorded 22 passing tests.
These results apply to the tested development sources, not to a future
release.

The CI bench gate passed in the same run, with almost no headroom: its
500-process D-state fixture measured a median 4.727 ms against a budget below
5 ms (1.06x). Earlier recorded runs measured 5.036 ms and failed. Green
runner medians on 6 November 2026 ranged from 3.227 ms (run 42174573944) to
4.993 ms (run 42166828185). The last recorded GUI validation failed
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
