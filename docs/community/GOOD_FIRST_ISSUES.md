# Good first issue drafts

These are drafts for the repository owner to review and post. They are not a
claim that the work has been accepted or scheduled. Keep each issue small; split
it if investigation uncovers a second problem.

## Owner TODO: capture a real GTK screenshot

This is not a contributor issue. Capture the real GTK app on a machine with a
working Wayland or X11 display, because the Broadway backend is limited to
1024×768 while the requested capture is 1280×800 (`PENDENCIAS.md`, CC7).

- Build and run the real GTK app from the captured commit.
- Capture at least one populated state at 1280×800; do not use fixture data that
  could be mistaken for a live session unless the caption says it is a fixture.
- Remove usernames, private project names, paths, hostnames, session IDs, and
  prompt content from the frame.
- Add the image under `site/img/`, show it in the README, and keep the CLI image
  as the headless example.
- Record the commit, capture command, desktop backend, and what was redacted.

## 1. Document every CLI subcommand in the README

**Suggested labels:** `good first issue`, `documentation`, `cli`

`USAGE` and `main` in `crates/agentcenter-cli/src/main.rs` list `snapshot`,
`cockpit`, `watch`, `signal`, and `reclaim`, while the README currently
introduces only `snapshot`. Add a short, factual command guide without
presenting session controls as stable.

### Acceptance criteria

- Add one sentence per subcommand, based on its `--help` text.
- Keep the existing `snapshot` examples and pre-alpha warning.
- If an `AGENTCENTER_EXTRA_AGENTS` example is added, use a process name without
  a built-in adapter, such as `my-agent`.
- Link to `agentcenter-cli <command> --help` instead of duplicating every flag.
- Run the documented commands with `--help` and confirm they exit successfully.

## 2. Clean up the Herdr test directory after a bind failure

**Suggested labels:** `good first issue`, `testing`, `rust`

The `fixture` helper in
`crates/agentcenter-core/src/control/herdr_tests.rs` creates its directory
before binding the socket. `PENDENCIAS.md` records that a bind failure can leave
that test directory behind.

### Acceptance criteria

- Add a regression test that forces the bind failure in a temporary directory.
- Make cleanup deterministic on both the success and failure paths.
- Assert that no test-owned socket or directory remains afterward.
- Do not broaden deletion beyond the exact temporary path created by the test.

## 3. Make `herdr_server` testable with a synthetic environment and proc root

**Suggested labels:** `good first issue`, `testing`, `refactor`, `rust`

`herdr_server` in `crates/agentcenter/src/actions_ui/session_control.rs`
reads `std::env` and `/proc` directly, so its server-selection behavior cannot
be exercised with temporary fixtures. Refactor only those two inputs behind a
small injected environment/proc-root boundary, preserving production behavior.

### Acceptance criteria

- Keep `herdr_server`'s production caller using the real environment and
  `/proc`.
- Test no server, one valid same-UID server, more than one valid server, and
  malformed or unreadable process entries with a temporary proc fixture.
- Assert the existing fail-closed errors: `no_herdr_pane`, `identity`, and
  `ambiguous` where applicable.
- Do not connect to a real Herdr socket or read the contributor's home config.
- Put the focused tests in the existing `#[cfg(test)] mod tests` in
  `crates/agentcenter/src/actions_ui/session_control.rs`.

## 4. Add a file-descriptor leak regression test for task scanning

**Suggested labels:** `good first issue`, `testing`, `performance`, `linux`

`count_d_threads` in `crates/agentcenter-core/src/procfs/task.rs` owns and
closes the task-directory and per-thread descriptors internally, but CC10 in
`PENDENCIAS.md` records that repeated scans still lack a leak regression test.

### Acceptance criteria

- Repeatedly scan a synthetic proc fixture and compare the process's open file
  descriptor count before and after.
- Include both a successful scan and an error path.
- Keep the assertion tolerant of the test harness's own stable descriptors but
  strict enough to catch growth per iteration.
- Keep this tests-only: do not refactor the scanner's `openat`/`getdents64` hot
  path or change its production behavior.
- Run the focused test repeatedly and document the command in the pull request.
