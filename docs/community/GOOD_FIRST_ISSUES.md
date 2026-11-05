# Good first issue drafts

These are drafts for the repository owner to review and post. They are not a
claim that the work has been accepted or scheduled. Keep each issue small; split
it if investigation uncovers a second problem.

## 1. Document every CLI subcommand in the README

**Suggested labels:** `good first issue`, `documentation`, `cli`

`crates/agentcenter-cli/src/main.rs` lists `snapshot`, `cockpit`, `watch`,
`signal`, and `reclaim`, while the README currently introduces only `snapshot`.
Add a short, factual command guide without presenting session controls as stable.

### Acceptance criteria

- Add one sentence per subcommand, based on its `--help` text.
- Keep the existing `snapshot` examples and pre-alpha warning.
- Link to `agentcenter-cli <command> --help` instead of duplicating every flag.
- Run the documented commands with `--help` and confirm they exit successfully.

## 2. Add the isolated Claude E2E command to CONTRIBUTING.md

**Suggested labels:** `good first issue`, `documentation`, `testing`

`PENDENCIAS.md` records that `scripts/e2e-claude-herdr.sh` is not cited in the
contributor guide. Document when and how it is safe to run.

### Acceptance criteria

- Add the exact command `bash scripts/e2e-claude-herdr.sh` to `CONTRIBUTING.md`.
- Explain that it uses an isolated Herdr, temporary home, and local provider
  stub; it must not target a live Claude session.
- State whether the script is part of the default `scripts/ci.sh` gate.
- Preserve all existing privacy and synthetic-credential rules.

## 3. Capture a real GTK screenshot for the website and README

**Suggested labels:** `good first issue`, `documentation`, `design`

The current website uses a real CLI capture because the documentation work ran
without a display. `PENDENCIAS.md` also records missing real GUI captures at
important sizes and states. The project should never substitute a mock-up.

> **Owner follow-up:** capture this on a machine with a working Wayland, X11, or
> Broadway display. The current headless worktree cannot produce it.

### Acceptance criteria

- Build and run the real GTK app from the issue's commit.
- Capture at least one populated state at 1280×800; do not use fixture data that
  could be mistaken for a live session unless the caption says it is a fixture.
- Remove usernames, private project names, paths, hostnames, session IDs, and
  prompt content from the frame.
- Add the image under `site/img/`, show it in the README, and keep the CLI image
  as the headless example.
- Record the commit, capture command, desktop backend, and what was redacted.

## 4. Prove Claude transcript content cannot enter a Snapshot

**Suggested labels:** `good first issue`, `testing`, `privacy`, `rust`

`PENDENCIAS.md` lists a missing test for absence of content in the Snapshot.
`CONTRIBUTING.md` defines the canary pattern used to prove that overlays expose
metadata only.

### Acceptance criteria

- Add a synthetic Claude transcript fixture containing one unique canary string
  in prompt, response, tool input, and tool output fields.
- Exercise the real Claude overlay path and assert that the canary appears in
  neither serialized Snapshot JSON nor `Debug` output.
- Assert that the overlay actually ran so the test cannot pass vacuously.
- Use no transcript copied from a live session.

## 5. Clean up the Herdr test directory after a bind failure

**Suggested labels:** `good first issue`, `testing`, `rust`

`PENDENCIAS.md` records that `herdr_tests.rs` can leave a directory behind when
socket binding fails.

### Acceptance criteria

- Add a regression test that forces the bind failure in a temporary directory.
- Make cleanup deterministic on both the success and failure paths.
- Assert that no test-owned socket or directory remains afterward.
- Do not broaden deletion beyond the exact temporary path created by the test.

## 6. Add focused tests for `herdr_server`

**Suggested labels:** `good first issue`, `testing`, `rust`

The CC7 notes in `PENDENCIAS.md` identify `herdr_server` as lacking direct test
coverage.

### Acceptance criteria

- Cover the no-server, valid-server, and malformed-or-unreadable configuration
  paths with temporary fixtures.
- Assert fail-closed behavior for data that cannot be trusted.
- Do not connect to a real Herdr socket or read the contributor's home config.
- Keep the tests in the nearest existing control test module.

## 7. Add focused tests for the control `Session` constructor

**Suggested labels:** `good first issue`, `testing`, `rust`

The CC7 notes in `PENDENCIAS.md` also call out the control-layer `Session`
constructor as untested.

### Acceptance criteria

- Cover the minimum valid session and each optional identity field.
- Include one mismatch or missing-identity case and assert the safe result.
- Use synthetic IDs, paths, and process data only.
- Avoid changing public behavior unless the new test exposes a documented bug.

## 8. Add a file-descriptor leak regression test for task scanning

**Suggested labels:** `good first issue`, `testing`, `performance`, `linux`

The CC10 notes in `PENDENCIAS.md` say task scanning lacks a file-descriptor leak
test.

### Acceptance criteria

- Repeatedly scan a synthetic proc fixture and compare the process's open file
  descriptor count before and after.
- Include both a successful scan and an error path.
- Keep the assertion tolerant of the test harness's own stable descriptors but
  strict enough to catch growth per iteration.
- Run the focused test repeatedly and document the command in the pull request.

## 9. Verify procfs descriptors are opened with close-on-exec

**Suggested labels:** `good first issue`, `testing`, `linux`, `rust`

The CC10 review notes in `PENDENCIAS.md` record missing `O_CLOEXEC` evidence for
the low-level task-directory scan.

### Acceptance criteria

- Add a Linux-only test that inspects the flags on the opened test descriptor.
- Assert `FD_CLOEXEC` without relying on a live `/proc` race.
- Keep the production path compatible with the documented glibc 2.30 minimum.
- Run formatting, clippy, and the focused test.

## 10. Refuse a non-directory `task/` fixture without blocking

**Suggested labels:** `good first issue`, `bug`, `linux`, `rust`

`PENDENCIAS.md` records that the low-level task scanner opens `task/` without
`O_DIRECTORY`; a FIFO in a synthetic proc root can block even though this does
not occur in real `/proc`.

### Acceptance criteria

- Add a regression fixture or temporary test where `task/` is a FIFO or regular
  file.
- Make the scan return a bounded, safe error instead of opening or blocking on
  the entry.
- Keep the normal real-`/proc` path unchanged and allocation-conscious.
- Confirm the test completes under a short timeout and run the tick benchmark.

## 11. Remove fixed machine paths from the Pi control Node tests

**Suggested labels:** `good first issue`, `testing`, `javascript`, `portability`

`PENDENCIAS.md` records Node tests with fixed paths outside the CI environment.
Replace one fixed-path assumption at a time with a test-owned temporary path.

### Acceptance criteria

- Identify the fixed path in the issue description before changing it.
- Create all test state under a unique temporary directory and clean it up in a
  `finally` path.
- Do not read or write a real Pi session, Herdr socket, or home configuration.
- Run `PI_OFFLINE=1 node --test extensions/pi-control/*.test.mjs` successfully.
