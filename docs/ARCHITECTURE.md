# AgentCenter — architecture (ADR-001)

Status: accepted · 2026-10-15

## Problem

A developer running AI coding agents in parallel (Claude Code, Codex, pi, Copilot
CLI, Kiro, Gemini, opencode…) has no way to see what each *session* costs the
machine. `htop`, Mission Center and Resources show processes, not sessions: a
single Claude session is a `claude` process plus a dozen children (MCP servers,
shells, test runners, builds). On the reference machine (16 cores, 38 GB RAM)
31 sessions used 8.3 cores and 18.5 GB, half of it in swap, with 24 of 27 GB of
swap taken — and nothing on screen said which sessions to close.

Existing tools (`MBrassey/agtop`, `ldegio/agtop`, the various `aitop`s) are
terminal dashboards focused on tokens and cost; they sum RSS (which double counts
shared pages), ignore swap, have no notion of I/O-bound, and none is a desktop app.

## Goal

A desktop resource monitor, in the spirit of Mission Center and Resources, whose
unit is the **AI session**: CPU, memory (PSS + swap), disk throughput and
I/O stall per session and per child process, live, while costing the machine
almost nothing.

Non-goals for v0.1: token spend dashboards (others do it well), remote machines,
GPU, per-process network bytes, Windows.

## Decisions

| # | Decision | Why |
|---|---|---|
| 1 | New project, Rust | Low and predictable overhead, no GC, single static core shared by every front end. |
| 2 | GUI first, GTK4 + libadwaita (gtk4-rs, libadwaita-rs) | Native on GNOME/Ubuntu, same stack as Mission Center and Resources, ~40–60 MB baseline, modern adaptive widgets. |
| 3 | Headless CLI ships alongside (`agentcenter-cli`) | Works over SSH and inside herdr, gives `--json` for scripts, and is the test harness for the core. |
| 4 | Linux `/proc` reader of our own, no `sysinfo` in the hot path | We need `smaps_rollup`, `io`, `delayacct_blkio_ticks` and per-tree attribution that generic crates don't expose; reading only what we need keeps a tick under a few ms. A macOS backend comes later behind the same trait. |
| 5 | Read-only and local by default | Never sends data anywhere. Transcripts are parsed for metadata only (model, token counts, timestamps, tool call ids as hashes), never for prompt or answer content. The cockpit (C21) keeps to the same rule: everything in it comes from those counts, the transcript's mtime and the declared provider. The handoff age comes from `readdir` + `stat` of the handoff directories only; no handoff file is ever opened or read. Amended by the owner on 17/10/2026 (C23): to detect a loop, the parser reads each tool call's arguments into the canonical form of `contexto.py`'s `norm_args` (`sort_keys`, cut at 400 characters), hashes it with the tool name into one `u64` and drops the text right away; only the last 30 hashes per session stay in memory, and only `max_streak` and `repeated_max` leave the core. No argument is ever stored in a `String` field or written to the snapshot. |
| 6 | Ghidra and REA are development tools, not dependencies | They help contributors discover where closed-source agents keep session state, so an adapter can be written. They are documented in CONTRIBUTING, never linked or shipped. |
| 7 | License MIT | Open-source distribution and embedding; preserve third-party licenses. Earlier revisions' dual-license grants remain valid. |

## Layout

```
crates/
  agentcenter-core/   library: sampler, session model, adapters, history, actions
  agentcenter-cli/    binary `agentcenter-cli`: snapshot --json, watch, table
  agentcenter/        binary `agentcenter`: GTK4 + libadwaita app
data/                 .desktop, metainfo, icons, GSettings schema
docs/
```

The GUI runs the sampler on its own thread and receives one immutable
`Snapshot` per tick over a channel; the UI thread never touches `/proc`.

## Session model

- **Agent process**: a process whose executable or script matches an adapter
  (e.g. `claude`, `codex`, `node …/pi`, `gemini`, `copilot`, `kiro-cli`).
- **Session** = one agent process + every descendant that does not itself
  belong to a deeper agent process. Children (MCP servers, shells, builds) are
  attributed to the nearest agent ancestor. In-process subagents are part of
  their parent session.
- **Overlays** add context without owning processes: herdr (workspace, tab,
  pane, agent status, terminal title), Claude's `~/.claude/sessions/<pid>.json`
  (session id, name, busy/idle/blocked), transcripts (model, tokens,
  context %, tool calls; busy/idle for Codex and pi).
- Sessions that exist without a live process (Claude background jobs) are
  listed with state and no resource numbers.

## Metrics (per process, aggregated per session and per machine)

| Metric | Source | Notes |
|---|---|---|
| CPU % | `/proc/<pid>/stat` utime+stime delta | % of one core; also shown as share of the machine. |
| Memory | `/proc/<pid>/smaps_rollup` `Pss` | Shared pages divided among sharers, so sums are honest. RSS is shown per process but never summed. |
| Swap | `smaps_rollup` `SwapPss` | The number that explained the reference machine. |
| Disk read/write B/s | `/proc/<pid>/io` `read_bytes`/`write_bytes` delta | Storage traffic; `rchar`/`wchar` kept as "syscall I/O". |
| I/O stall % | `stat` field 42 `delayacct_blkio_ticks` delta | Needs `kernel.task_delayacct=1`; the app detects it and explains how to enable. Fallback: count of threads in `D` state. |
| I/O-bound flag | stall % high while CPU % low | Headline signal for "session waiting on disk". |
| Threads, FDs, ctx switches | `status`, `fd/` count, `status` | Cheap, shown in detail view. |
| Machine | `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/pressure/{cpu,memory,io}` | PSI shows contention the per-process view can't. |

Sampling (tick = 1 s by default; "full scan" = every 5 s, `smaps_idle_every`):

- Process list (readdir) every tick. `stat` of every pid only on the full
  scan; between scans, `stat` only for new pids and for the processes that
  were in a session on the previous tick, unless that session is quiet (see
  below). CPU, state, threads and **RSS** (field 24 of `stat`, in pages) come
  from that `stat`, so RSS is current every tick for processes of active
  sessions. `io` of those processes every tick.
- `status` (uid, `VmSwap`, ctx switches) only on the full scan, for a new
  process (or reused pid) and every tick for the selected session. Elsewhere
  those values can be **up to 5 s stale**; that lag is accepted.
- `smaps_rollup` (PSS, SwapPss) is expensive for the kernel (it walks the page
  tables: ~5 ms for a 400 MB process, ~10 ms at 500 to 800 MB, ~0.6 ms on
  average over session processes). Selected session: every tick. Other
  sessions: every 5 s if RSS (from `stat`) or swap (from the last `status`)
  moved by more than 1/256 since the last read; if they did not move, at most
  every **300 s** (`smaps_max_age`, spread by pid so the rereads do not land on
  the same tick). A new process is read at once.
- Without delay accounting, the `D`-thread count scans `task/*/stat` on the
  same 5 s cadence (every tick for the selected session). A single-threaded
  process and the main thread of a multi-threaded one take their state from
  the process `stat` already read, so `task/<pid>/stat` is never reread.
- **Quiet session** (C6c): on its last read every process had a previous read,
  the session's CPU (summed over its processes, over the window since that
  read) stayed **below 1 % of one core** (`QUIET_MAX_CPU_PCT`), none moved
  `read_bytes`/`write_bytes` or `delayacct_blkio_ticks`, and none was in `D`
  (the process, or a thread in the `task/` count). The CPU allowance is the
  owner's call (2026-10-16): an idle agent still wakes on timers and polls, so
  requiring an unchanged CPU counter kept almost every session awake; disk and
  stall still require an unchanged counter. `D` is there on purpose: a session stuck in swap-in has
  ~0 CPU and 0 `read_bytes` but is waiting on the disk. A quiet session that is
  not selected is read only on the full scan: between scans its pids keep the
  cached `stat` and each process republishes its last metrics, with no `io`,
  `status`, `smaps_rollup` or `task/` read. A pid that was not in the previous
  tick (the readdir still runs every tick) is read at once and wakes the whole
  session; active and selected sessions are read every tick. Between reads a
  quiet session shows the mean of its last window (under 1 %, not 0) for **up
  to 5 s** after it wakes, and a reused pid inside it shows up at the next full
  scan. Rates (CPU %, B/s, stall) are taken over
  the real interval since that process was last read (`cpu.total` stored with
  the read), so the first read after a quiet period is the mean of the period,
  not a false peak. The rereads are aligned with the full scan, not spread by
  pid as the C6c card first asked: that tick already reads every `stat`, and
  spreading them would read the `stat` of those pids twice every 5 s. The
  forced `smaps_rollup` reread keeps its per-pid turn: a turn that fell
  between two reads of a quiet process is taken at the next read, so quiet
  sessions do not all reread at 300 s. The metrics republished between reads
  are copied only for sessions that end the tick quiet, so the worst case
  (every session active) pays no copy.
- `cmdline` and `cwd` of the session's agent: when the agent appears (new or
  reused pid), on the full scan, and every tick for the selected session.
  Elsewhere the cached value is published, **up to 5 s stale** (an agent that
  `chdir`s shows the new `cwd` at the next full scan).

Measured cost (C6b, 2026-10-15, 16 cores, under load 4 to 34 from other
sessions). Tick over the 500-process fixture: `tick_fixture_500_delayacct`
2.50 ms median, `tick_fixture_500_d_state` 4.19 ms; both are gated at < 5 ms,
the `D`-state path with only ~1.2x headroom. `agentcenter-cli watch` on this
machine (~850 processes, ~300 in sessions): **1.3 to 2.1 % of one core** (13
to 21 ms/s) and 4.0 to 4.4 MB RSS; with the machine quieter (load 1.8 to 2.9)
the review measured 0.87 and 0.90 %. CPU is over the 0.5 % budget: about 90 % of
it is kernel time spent producing `/proc` files, mostly the per-tick `stat`
and `io` of session processes (~460 and ~300 opens per tick); `smaps_rollup`
is no longer the main cost after the first tick.

Quiet sessions (C6c, 2026-10-16, ~850 processes, 27 sessions, ~280 session
processes, load 17 to 34). Opens counted with `ProcRoot::counting("/proc")`
over 10 live ticks: **293 `stat` and 180 `io` per tick** on average, against
~449 and ~280 before (-35 % of those opens). Between full scans only 73 to
125 `stat` per tick; what remains is the full scan itself (~843 `stat` every
5 s, ~169 per tick) and the wake-up after it: over a 5 s window almost every
idle Claude session has moved at least one CPU tick, so it is not quiet on the
full scan and is read again on the next tick or two. `agentcenter-cli watch`
with the machine at load 17 to 34: 2.37 to 3.33 % of one core, 4.4 to 4.8 MB
RSS; the C6b binary measured alongside it under the same load gave 3.8 and
5.8 %, so at that load the CPU figure is noise and the 0.5 % budget could not
be checked.

History: ring buffer per session, 10 min at 1 s, `f32`.

## Summary (Resumo, C13)

`Snapshot.summary` (snapshot schema 4) holds counts and sums over one tick.
`Summary::of` computes it once per tick in `sampler.rs`, right after
`overlay::apply`, from the sessions that tick already built. It does no
`/proc` or file read of its own: `tests/summary.rs` checks that the opens per
tick (`ProcRoot::counting`) are the same with and without it. `snapshot` and
`watch` print it as the first line of the table:

```
31 sessions (claude 24, codex 5, pi 2) | 4 busy, 2 blocked | 12 active processes
```

Agents are listed in alphabetical order, so the line stays put between
`watch` frames. The line goes through the same `sanitize` as the cells,
because an agent id can come from `AGENTCENTER_EXTRA_AGENTS`.

| Field | What it counts | Source |
|---|---|---|
| `sessions` | sessions with `role` agent | `Session.role` |
| `multiplexers` | sessions with `role` multiplexer (herdr) | `Session.role` |
| `by_agent` | agent sessions per adapter id; sums to `sessions` | `Session.agent` |
| `status.{busy,blocked,idle,unknown}` | agent sessions by declared state; sums to `sessions` | Claude's sessions file (`overlay.status`) first; otherwise the herdr pane status, when it is a value herdr publishes (`working` -> busy, `blocked` -> blocked, `idle`/`done` -> idle; one list, `overlay::herdr_state`); otherwise, for Codex and pi, the transcript (an open turn or a last answer that asked for a tool is busy), which fills `overlay.status` only when neither `overlay.status` nor a recognized pane status is set (a pane value outside the list is not a declared state, so the transcript fills); otherwise unknown. It is never inferred from CPU. |
| `sessions_with_working_children` | agent sessions with at least one working child | measured, not declared: it is not `status.busy`, and the two can differ either way (a busy agent thinking with no child, an idle agent with a background build) |
| `child_processes` | processes of every session (agents and herdr) except the session's own agent pid | `Session.processes` |
| `working_child_processes` | of those, the ones in state `R` or `D` or at CPU >= 1 % (`WORKING_CPU_PCT`); the CLI calls them "active processes" | process `state` and `cpu_pct` (from `stat`) |
| `io_bound_sessions` | sessions with the I/O-bound flag | `Session.io_bound` |
| `sessions_cpu_pct`, `sessions_pss_bytes`, `sessions_swap_pss_bytes` | sums over the sessions that have the value; `null` if none has it | `Session.cpu_pct`, `pss_bytes`, `swap_pss_bytes`. These are the TOTAL row's numbers, but with no partial marker. |
| `tool_calls_open`, `tool_calls_per_min` | sums over the sessions that have the value; `null` if none has it | `SessionOverlay.tool_calls_open` (tool calls without a result yet, at most 256 ids kept per transcript) and `tool_calls_per_min` (calls in the last 5 one-minute buckets, by the timestamp of the transcript line, divided by the time those buckets cover). The transcript overlay also fills `SessionOverlay.tool_calls` (total). All three appear only after the transcript has been read to the end once, like the token totals. |
| `subagents_live` | sum over the sessions that have the value; `null` if none has it | `SessionOverlay.subagents_live`, filled with `subagents_total` by the transcript overlay (C15), metadata only. Claude: the `agent-*.jsonl` files in `<sid>/subagents/` next to the session transcript (total = the files; the `.meta.json` is never opened); live = the last `assistant` line in the last 64 KiB has no `end_turn` and the file changed less than 120 s ago (no `end_turn` and an old mtime is an orphan, not live). The directory is listed again only when its mtime changes; a file that ended more than 10 s ago is read once and never again (not even `stat`), unless its `.meta.json` changes inode in a new listing, which Claude does when it resumes a subagent. First reads share 512 KiB per tick; until every file was read once, live is `null` and total is already set. Codex: rollouts in the parent's day directory (or a later one) whose `session_meta.source.subagent.thread_spawn.parent_thread_id` is the parent's id; live = an open turn (`task_started` with no later `task_complete`/`turn_aborted`) and an mtime under 120 s. pi: `null`. |

Staleness: the summary is exactly as fresh as the sessions it reads.
- **Quiet sessions lag by up to 5 s.** A quiet session is read again only on
  the full scan, and between reads its processes republish their last state
  and CPU (see Sampling). So a child that already exists and starts working
  can take up to 5 s to count in `working_child_processes` and
  `sessions_with_working_children`. A new pid is counted at once, because it
  wakes the session.
- PSS and swap sums follow the `smaps_rollup` cadence, so they can lag by up
  to 300 s when memory does not move.
- The declared state (`status`) is whatever the overlays published on that
  tick.

## Cockpit (C21)

Snapshot schema 5 adds, per agent session, the handoff cockpit of the squad's
`contexto.py` (`skills/contexto-de-sessao/scripts` in agentic-swarm):
`SessionOverlay.provider`, `SessionOverlay.cockpit` and
`SessionOverlay.cockpit_pending`, and five aggregates in the summary. The
formulas live in `cockpit/mod.rs` (`Thresholds::contas`,
`Thresholds::recommend`) as a copy of the script's `contas` and `analyse`:
same inputs, same result, same order of floating-point operations, Python's
`round` (`round_n`) and `int` truncation. Where the script does something odd
the module copies it and says so; it does not reinterpret it. JSON written as
schema 4 still reads, with these fields `null`, zero or `false`.

**Source.** The transcript overlay, the same incremental scan that fills
`tokens_in` and `context_pct` (see the Summary table). The token totals cover
the whole transcript: `/compact` does not reset them and `/clear` starts a new
file. Raw inputs, per transcript: `input` (uncached), `cache_write`,
`cache_read`, `output`, the context of the last request `ctx`, and the
`window` (declared by the transcript or inferred from `KNOWN_WINDOWS`).

**Formulas** (weights `W_WRITE` = 1.25, `W_READ` = 0.1, `W_OUT` = 5):

- `novo = input + 1.25*cache_write + 5*output`; `relei = 0.1*cache_read`;
  `burn_weighted = int(novo + relei)`.
- `burn_pct = round(100*(novo + relei) / CLAUDE_SESSION_BUDGET, 1)`.
- `reread_share = round(relei / (novo + relei), 3)`;
  `reread_ratio = round(relei / novo, 3)`.
- `cache_hit = round(cache_read / (input + cache_write + cache_read), 3)`, in
  raw tokens; this is what the owner reads, but it is the weighted
  `reread_share`, not `cache_hit`, that decides a HANDOFF.
- `floor_tokens` (the reread floor): the floor % slides from `SQUAD_CTX_PCT`
  down to `min(SQUAD_REREAD_PISO, SQUAD_CTX_PCT)` as `burn_pct` goes from 0 to
  `SQUAD_BURN_CHEIO`, then `max(2*SQUAD_CTX_BASE, int(window * floor % / 100))`.
- `payback_turns = round((1.25*SQUAD_CTX_BASE + 5*2000) / (0.1*(ctx - SQUAD_CTX_BASE)), 1)`,
  `null` when `ctx <= SQUAD_CTX_BASE`.
- `idle_min`: minutes since the transcript's mtime. `handoff_age_min` and
  `handoff_name`: the newest `.claude/handoffs/*.md` walking up from the
  session cwd to the repo root, else the matching `~/.claude/handoffs/*.md`
  (`contexto.latest_handoff`); the name is stripped of ANSI and control
  characters and cut at 120 characters. The core walks up from the
  process cwd (`Session.cwd`, `/proc/<pid>/cwd`); the script walks up from
  the cwd the transcript records. Usually both are the same path. They can
  differ in a git worktree: the
  process can sit in the worktree while the transcript records the main
  repo, or the reverse, and the walk stops at a different `.git` on each
  side (a worktree stops at its own `.git` file), so each can pick a
  different handoff, or one finds none. Seen live on 17/10/2026 in one of
  26 sessions, a session in a worktree.
- `recommendation`, in `analyse()` order: OCIOSA if `idle_min > 90`; HANDOFF
  if context % >= `SQUAD_CTX_PCT`, else COMPACTAR if >= 50 and nothing set it
  yet; HANDOFF if `reread_share >= SQUAD_REREAD` (unrounded) and
  `ctx >= floor_tokens`, unless the verdict is already OCIOSA or LOOP. `reasons`
  carries the numbers behind each step (and an old handoff, > 240 min, with
  context >= 50 %); the text is the front end's. LOOP is in the contract and
  the summary.
- LOOP (C23): `cockpit.max_streak` (longest run of identical calls in a row)
  and `cockpit.repeated_max` (most times one call appears) over the last 30
  tool calls, as `contexto.loops()`; a call is the hash of the tool name and
  the canonical arguments (see decision 5). LOOP when either reaches 3. It is
  checked after OCIOSA and before the context steps, as in the script: it wins
  over OCIOSA, urgent context (HANDOFF) still wins over it, and COMPACTAR
  never replaces it. `max_streak` is 1 for
  a transcript with no calls (the script's `best = 1`); 0 means no signal (JSON
  from before C23). A call whose arguments cannot be read (lone surrogate,
  number out of `f64` range) still counts as a line and stays out of the
  window.

The thresholds come from the process environment once, when the overlay is
built, with the script's defaults and clamps (`SQUAD_CTX_PCT` 90,
`SQUAD_REREAD` 60, `SQUAD_REREAD_PISO` 70, `CLAUDE_SESSION_BUDGET` 20M,
`SQUAD_BURN_CHEIO` 50, `SQUAD_CTX_BASE` 40000). `cockpit.cache_write` and
`cockpit.ctx_tokens` ship with the rest so a reader can redo the sums from the
JSON with `tokens_in` and `tokens_out`, without the transcript.

**Provider** (`cockpit::provider`, `[a-z0-9._-]{1,32}`): the one the transcript
declares (pi's `provider`, Codex's `session_meta.model_provider`), lowercased,
with `openai-codex` read as `openai`; a declared value outside the alphabet is
dropped whole, not sanitized. Otherwise the agent's (`claude` -> anthropic,
`codex` -> openai, `gemini` -> google, `copilot` -> github, `kiro` -> aws,
`cursor-agent` -> cursor), otherwise the model prefix (`claude-`, `gpt-`,
`o1`/`o3`/`o4`, `codex-`, `gemini-`, `<vendor>/`), otherwise `null`. It is set
for every agent session, with or without a matched transcript.

**Summary fields** (agent sessions only; herdr never counts):

| Field | What it is |
|---|---|
| `cockpit.{continuar,compactar,handoff,ociosa,loop,sem_dado}` | agent sessions by `cockpit.recommendation`; `sem_dado` is a session without `overlay.cockpit`. Sums to `sessions`. |
| `cockpit_pending` | of the `sem_dado`, those with a matched transcript whose first full scan has not finished (`overlay.cockpit_pending`) |
| `sessions_burn_weighted` | sum of `cockpit.burn_weighted`; `null` if no session has a cockpit |
| `cache_hit` | sum of `cockpit.cache_read` / sum of `overlay.tokens_in` over the sessions with a cockpit, 3 places; weighted by tokens, not the mean of the per-session `cache_hit`; `null` without tokens |
| `ctx_pct_max` | the highest `overlay.context_pct` (1 place, as published); `null` if none has it |

Staleness:
- `overlay.cockpit` appears only after the transcript has been read to the
  end once, like `tokens_in`: a large transcript is read at most
  `READ_BUDGET` (1 MiB) per tick across all files, so the first cockpit of a
  GB-sized transcript can take many ticks. Meanwhile `cockpit_pending` is
  `true`.
- The cockpit is recomputed when its transcript is checked, at most every
  `REFRESH_EVERY` (5 s); in between the tick republishes it as it was. So the
  tokens, `idle_min` and the verdict lag by up to 5 s, and an OCIOSA verdict
  leaves up to 5 s after the session writes again.
- The handoff lookup is cached per cwd for the same 5 s, with its age moved
  forward by the wall clock. A directory listing is reused until the
  directory's mtime changes (and is at least 2 s old); every `.md` is
  `stat`ed again on each lookup, so a rewritten handoff is seen on the next
  one. At most 256 listings are kept (LRU).

## Actions

Suspend (SIGSTOP), resume (SIGCONT), terminate (SIGTERM), kill (SIGKILL), on a
session (whole tree) or one process. Always confirmed, only for processes owned
by the current user, never on the app itself. "Reclaim" view lists idle
sessions sorted by memory + swap so the user decides what to close. Idle uses
the summary's declared state (`session_state`, same precedence as
`status.{busy,blocked,idle,unknown}`): idle enters; busy or blocked, from the
overlay or from the herdr pane (`working`, `blocked`), never enters whatever
the CPU; only an unknown state falls back to CPU under 1 % on every tick of the
window.

Accepted risk (owner, 17/10/2026): the test probe `AGENTCENTER_ACTION_PROBE=
<stop|cont|term|kill>:<session|pid>:<PID>:<confirm|cancel|hold>` is read by the
release binary too. With `confirm` it answers the same confirmation dialog
without a click, so anyone who can set the app's environment can send a signal
to any process of the same user. It stays because it is the only way to drive
the action flow end to end on a machine without an input injector (ydotool,
wtype); the core `Guard` still refuses pid 1, the app itself, herdr and other
users' processes, and the actions are never automatic otherwise.

## Opt-in session control (CC1)

`agentcenter-core::control` is an explicit control boundary, separate from the
sampler and GTK thread. Pi discovery reads bounded private manifests; execution
uses protocol 2 over Unix sockets with nonce, UID/peer PID and kernel starttime
checks. An operation retains its confirmed manager/session/file/leaf/revision
snapshot and UUID through accepted, running and terminal results. A timeout
following send is unknown and must be queried by that same UUID, not retried as
a new write. No control runs automatically from a threshold.

The opt-in [Pi extension](../extensions/pi-control/README.md) owns effects in a
private CC0 runtime. Handoff generates a bounded summary with the current
provider and saves it privately with git ground truth; it never changes the
session lifecycle. Compact calls the guarded native API and confirms the saved
entry. Both require authenticated-model capability and explicit UI consent for
the target, revision, effect and possible cost. AgentCenter receives metadata,
not conversation or summary content. Metadata-only handoff discovery matches
the exact native session ID and ignores symlinks and files owned by another UID.

Herdr focus uses a separate adapter and explicit workspace/tab/pane IDs. It
revalidates process identity and mapping before focusing, then checks the
semantic response. It never sends a prompt or claims desktop-window activation.
There is no Herdr server-side process CAS. Pi socket DAC/nonce and client
SO_PEERCRED do not isolate hostile processes running as the same UID or root.
The linked contract documents bounds, activation, test inputs and cancellation.
No extension is installed or attached to an existing session automatically.

## Claude Code control through Herdr (CC4)

The common contract lives in `control/mod.rs`: the CC1 states
(accepted/running/completed/rejected/failed/cancelled/unknown, where only
unknown is non-terminal), `Capability{enabled, disabled_reason,
cancel_supported}`, a canonical lowercase UUID per operation, closed codes, and
evidence that carries path, time, size and sha256, never content. `pi.rs`
re-exports `State` and `Capability`; the Pi protocol 2 wire is unchanged.
`control/pickup.rs` only declares the composite handoff-pickup stages
(`handoff_running` -> `handoff_persisted` -> `clearing` ->
`new_session_confirmed` -> `pickup_sent` -> `pickup_loaded`) and the old
session reference for manual resume; nothing implements them yet.

`control/claude.rs` enables Handoff and Compact only when the session is
`claude`, maps to an explicit Herdr workspace/tab/pane, `/proc` starttime,
`pane.process_info` and `agent.get` agree on pid and session ID, the agent is
`idle`, the transcript resolves and no squad-vigia claim
(`~/.claude/squad/trocas/<session>.claim`, stat only) exists. The Herdr socket
must be a 0600 socket of the UID under ancestors that are not group- or
world-writable (the default `~/.config/herdr` is created 0775 and is therefore
refused until tightened). The adapter, not the client, chooses the text:
`/compact`, or `/clear-pickup handoff agentcenter-op:<operation_id>`. Busy or
another operation in flight for the session rejects; there is no queue and no
`/clear`.

Completion is by evidence, read from the transcript only when the operation
starts or is queried, from the byte offset it had just before the send:

- Compact: a `system`/`compact_boundary` entry of the same `sessionId` with
  `compactMetadata.trigger == "manual"`, timestamped at or after the send and
  the operation's `/compact` user entry. Claude writes that user entry after
  the boundary in file order, so the order is by timestamp. Auto boundaries and
  earlier boundaries never count.
- Handoff: the user entry carrying the operation mark, then a `Write` whose
  result succeeded for a path under `<git-root>/.claude/handoffs/`,
  `~/.claude/handoffs/` or `~/.claude/squad/handoffs/`; the file must be
  regular, reached without any symlink, owned by the UID and modified at or
  after the send. Size and sha256 are recorded; the bytes are hashed and
  dropped.

No evidence within the evidence timeout is `unknown`; a later status with the
same operation ID re-reads that operation's evidence only, and an `unknown`
operation still blocks new ones on the session for a hold period. A repeated
operation ID with the same action and session never resends. Local telemetry
keeps action, code, duration and a truncated sha256 of the operation ID.

Residual risk: Herdr has no compare-and-swap and answers one request per
connection. Revalidation (`/proc` starttime, `agent.list` +
`pane.process_info`, `agent.get` session and `idle`) runs on fresh
connections to the same pinned server (peer pid and starttime checked on each)
immediately before `agent.prompt`, but the pane can still change between the
last read and the prompt being typed. The window is a few milliseconds; a
prompt that lands in another session cannot complete this operation, because
evidence is matched by session ID and operation mark. A `herdr` reply to
`agent.prompt` only means the text was typed.

## Performance budget (measured, gated in CI where possible)

| Target | Budget |
|---|---|
| One core tick, 500 processes | < 5 ms CPU |
| `agentcenter-cli watch` at 1 Hz | < 2 % of one core, < 10 MB RSS (raised from 0.5 % by the owner, 2026-10-16) |
| GUI open, 1 Hz | < 2 % of one core, < 80 MB PSS |
| GUI cold start to first frame | < 300 ms |

Baseline, same machine, 2026-10-15, 5 s sample while idle on screen:

| App | CPU (% of one core) | PSS | SwapPss |
|---|---|---|---|
| Mission Center 1.2.0 (flatpak, + magpie helper) | 43 % | 492 MB | 226 MB |
| Stacer | 10 % | 145 MB | 0 |
| Resources (+ resources-processes helper) | 7 % | 137 MB | 0 |

Mission Center and Resources both gather data in a separate helper process.
We keep the sampler in-process on its own thread (one binary, no IPC
serialization) and revisit only if privilege separation is needed. Their
source (GPL-3.0) is reference for ideas only; no code is copied into this
MIT/Apache project.

## Adapters (v0.1)

| Agent | Detection | Overlay data |
|---|---|---|
| Claude Code | `claude` binary / node script | `~/.claude/sessions/<pid>.json`, `~/.claude/projects/*/*.jsonl` |
| Codex | `codex` | `~/.codex/sessions/**/rollout-*.jsonl` |
| pi | `pi` (node) | `~/.pi/agent/sessions/*/*.jsonl` |
| herdr | `herdr` server (measured as its own row) | `herdr agent list` / socket API: workspace, pane, status, title |
| Gemini, Copilot CLI, Kiro, opencode, Aider, Goose, cursor-agent | executable match | detection only in v0.1; overlays as contributed |

Adapters are a trait plus a data table, so adding an agent is one file and one
test fixture.
