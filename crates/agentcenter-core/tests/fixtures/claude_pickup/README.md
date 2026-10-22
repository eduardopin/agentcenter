Synthetic Claude Code transcript entries for the CC6 handoff-pickup tests
(`tests/control_claude_pickup.rs`). One JSON entry per line; the test replaces
`@SID@`, `@OP@`, `@PATH@` (prompt argument), `@READ@` (path read), `@ID@` and `@TS<n>@` (send time + n ms) before
appending. Shapes follow what Claude Code 2.1 writes for a slash command, a
`Write` and a `Read` (`toolUseResult.filePath` / `toolUseResult.file.filePath`).
`AGENTCENTER-CANARY-cc6` marks content that must never reach a Progress,
telemetry or Snapshot.
