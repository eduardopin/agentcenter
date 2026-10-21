# AgentCenter — visual direction (C16)

Status: proposed · 2026-10-17 · waiting for the owner's approval before C17
(gauges) and C18 (rich rows) leave TODO.

The owner's brief (2026-10-17): "the graphics are weak; modernize it, it looks
like a spreadsheet", with gauges for sessions, agents, subagents, parallel
activity and tool calls. This document is the direction; the widgets come in
C17 and C18. The tokens in `crates/agentcenter/resources/tokens.css` and the
preview applied to the C10 window are the only code change here.

## 1. Scene of use

One developer, one workstation (16 cores, 38 GB RAM), 20 to 40 coding agents
running at once in herdr panes. AgentCenter sits on a side monitor or half the
screen. It is read in glances, dozens of times an hour, with two questions:

1. **Does the machine hold?** CPU, RAM and swap, and how much of each the
   agents take.
2. **Which session needs me?** Blocked sessions, the ones eating memory, the
   ones stuck on disk.

Mode: **Operate**. Scannable, consistent, native to GNOME; the identity lives
in the detail, not in decoration. Light or dark follows the system
(`AdwStyleManager`): the scene runs day and night next to a terminal.

## 2. Direction

**Subject.** The kernel's own accounting (PSS, swap, PSI) and the agents'
work loop (busy, blocked on you, idle, a tool call in flight). What no other
monitor shows is *how much of this machine the agents take*. That is the
signature.

**Signature: the machine rule.** CPU, RAM and swap are each one horizontal
rule whose filled part is split in two: the agents' share (amber) and the rest
of the machine (slate), with the free capacity as an empty track. One look
says "the agents hold 20 of the 29 GiB in use". Everything else stays quiet.

**Colour.** Six roles; hex values and contrast in `tokens.css`.

| Name | Light | Dark | Role |
|---|---|---|---|
| Graphite / Ice | `#eef0f4` window, `#f7f8fa` view, `#ffffff` panel | `#15161c` window, `#101116` view, `#1c1d24` panel | surfaces; cool, never cream |
| Ink | `#15161c`, secondary `#474a58` | `#e9eaf0`, secondary `#a7aab8` | text |
| Amber | `#9a5800` fill, `#7a4300` text | `#f2a93f` | **work in progress**: the agents' share of a rule, a busy session, the focus ring, the selection tint. Fixed accent, not the system one |
| Crimson | `#b3123f` | `#ff7590` | **needs you**: blocked session, pressure over its limit, I/O-bound |
| Slate | `#6b6f80` | `#858a9c` | the rest of the machine in a rule |
| Track | `#dfe2e9` | `#30323d` | empty capacity |

Why a fixed accent: in a monitor colour is meaning. Ubuntu's default accent is
orange, which would collide with any warning colour and would make "busy" and
"pressure" the same hue. Warning is therefore remapped to crimson, and amber
means only "work happening".

**Type.** The platform's faces, on purpose: the system UI font (Ubuntu Sans
here, Adwaita Sans on stock GNOME) for everything, with tabular numerals on
every number; the system monospace only for data that is code (paths, pids,
commands). Weight carries the hierarchy: a readout is 700 at 1.4em, its
capacity ("of 38.0 GiB") stays regular and secondary. No bundled face: a
native monitor that ignores the user's font settings reads as a web page. If
the owner wants a face of its own, that is an open decision (section 8).

**Motion: none.** The window updates at 1 Hz and is looked at dozens of times
an hour; an animated fill would be constant movement in the corner of the eye
and paint cost on the cairo renderer, against a 2 % CPU budget. Values jump to
the new reading. Reduced motion holds trivially.

**Structure.** No eyebrow labels over titles, no numbered sections, no row of
equal cards. The rule's label sits on the left of the rule, the readout on the
right; counters are one line of text, not tiles.

## 3. Two layouts

Both keep the header bar (title and "N sessions · M of P processes") and the
banner for the D-state fallback. Wireframes in the app's language (English).

### A. Instrument cluster (dials)

A row of round gauges on top, counters under them, the list below.

1280 × 800
```
┌ AgentCenter · 38 sessions · 291 of 861 processes ─────────────────────┐
│  ╭─────╮    ╭─────╮    ╭─────╮    ╭─────╮    ╭─────╮                   │
│ │  42% │  │  77% │  │  73% │  │ 12/16│  │   7  │                   │
│  ╰ CPU ╯    ╰ RAM ╯    ╰ Swap╯    ╰Paral╯    ╰Tools╯                   │
│  38 sessions: claude 31 · codex 5 · pi 2   9 busy · 3 blocked           │
├────────────────────────────────────────────────────────────────────────┤
│ ▸ kiln-cache-layer    claude  Busy   14%   728 MiB   ctx 86%   ~/src/kiln│
│ ▸ ledger-i18n         claude  Idle   30%   719 MiB   ctx 71%   ~/src/led…│
│   …                                                                    │
```
840 × 700
```
┌ AgentCenter · 38 sessions ─────────────────────┐
│ ╭───╮ ╭───╮ ╭───╮ ╭───╮ ╭───╮                   │
│ │42%│ │77%│ │73%│ │12 │ │ 7 │                   │
│  CPU   RAM  Swap  Par.  Tools                   │
│ 38 sessions · 9 busy · 3 blocked                │
├─────────────────────────────────────────────────┤
│ kiln-cache-layer  Busy  14%  728 MiB  ctx 86%   │
```
360 × 640
```
┌ AgentCenter ───────────┐
│ ╭───╮ ╭───╮ ╭───╮      │
│ │42%│ │77%│ │73%│      │
│ ╭───╮ ╭───╮            │
│ │12 │ │ 7 │            │
│ 38 sess · 9 busy · 3 bl│
├────────────────────────┤
│ kiln-cache-layer  Busy │   ← first row at ~330 px
```

### B. Machine rule and session rack (chosen)

Three rules (CPU, RAM, swap), each split agents / rest / free, with the readout
and capacity in text; one line of counters; the list grouped by state, blocked
first.

1280 × 800
```
┌ AgentCenter · 38 sessions · 291 of 861 processes ─────────────────────┐
│ CPU   ████████▒▒▒▒▒░░░░░░░░░░░░░░░░░░░░   42% of 16 cores   agents 35% │
│ RAM   ██████████████▒▒▒▒▒▒▒░░░░░░░░░░░░   29.3 of 38.0 GiB  agents 20.1│
│ Swap  ████████▒▒▒▒▒▒▒▒▒▒▒▒░░░░░░░░░░░░░   19.7 of 27.0 GiB  agents 11.0│
│ 38 sessions  claude 31 · codex 5 · pi 2 │ 9 busy · 3 blocked · 26 idle  │
│ 76 active of 16 cores │ 7 tool calls in flight · 42/min │ 3 subagents   │
├────────────────────────────────────────────────────────────────────────┤
│ Blocked · 3                                                            │
│ [!] nimbus-export-csv        claude · ~/src/nimbus · w2/p20            │
│     Blocked   CPU ▮▯▯ 23%   704 MiB + 582 swap   ctx ▮▯▯▯ 13%   2 tools │
│ Busy · 9                                                               │
│ [>] kiln-auth-refactor       claude · ~/src/kiln · w1/p9               │
│     Busy      CPU ▮▮▯ 16%   734 MiB + 174 swap   ctx ▮▮▮▮ 82%   1 tool  │
│ Idle · 26                                                              │
```
840 × 700
```
┌ AgentCenter · 38 sessions ─────────────────────┐
│ CPU  ██████▒▒▒░░░░░░░░░░   42% of 16 cores     │
│ RAM  ██████████▒▒▒▒▒░░░░   29.3 of 38.0 GiB    │
│ Swap ██████▒▒▒▒▒▒▒▒░░░░░   19.7 of 27.0 GiB    │
│ 38 sessions · 9 busy · 3 blocked · 76 active   │
│ 7 tool calls in flight · 3 subagents           │
├────────────────────────────────────────────────┤
│ Blocked · 3                                    │
│ nimbus-export-csv          Blocked  704 MiB    │
│ claude · ~/src/nimbus      CPU 23%  ctx 13%    │
```
360 × 640
```
┌ AgentCenter ───────────┐
│ CPU  ████▒▒░░░░   42%  │
│ RAM  ███████▒▒░ 29/38G │
│ Swap ████▒▒▒▒░░ 20/27G │
│ 38 sessions claude 31… │
│ 9 busy 3 blocked 26 idl│
│ 80 active of 16 cores …│
│ Pressure memory ▮ I/O ▮│
│ Tool calls … Subagents │
│ Cockpit … (wraps)      │
├────────────────────────┤
│ Blocked · 3            │
│ nimbus-export-csv      │   ← first row at ~360 px
│ Blocked · 704 MiB · 23%│
```

Measured in C17 (`capture-gui.sh` prints `band_px`, the height of the band):
139 px at 1280, 163 px at 840, 179 to 197 px at 360 (no sessions / 38
sessions). With the cockpit counters wired in C24 (handoff, loop, compact,
idle, ok, reading, no data, burn, cache hit, context max) the band measures
139 to 163 px at 1280, 183 px at 840 and 215 px at 360, still with every
counter shown. The ~150 px planned for 360 assumed the band would drop tool calls,
subagents and the cockpit there; the C17 review rejected hiding them (the
card asks for a stacked band, and a screen reader must reach every counter).
So at 360 the counter groups stack and wrap inside themselves, with tighter
spacing, a 1.15em readout and 0.9em counters; only the "agents N" column of
the rules leaves, its share stays in the amber segment and in the spoken
value of the rule.

### Comparison

| | A. Dials | B. Rule and rack |
|---|---|---|
| Answers "does the machine hold?" | yes, one number per dial | yes, and **who** holds it: agents vs the rest, which a dial cannot split |
| Answers "which session needs me?" | no: list order is by memory | yes: blocked sessions open the list |
| Height of the top band | ~150 px at 1280, ~330 px at 360 (dials stack) | ~110 px at 1280, ~150 px at 360 (rules stay one line each); measured in C17: 139 px at 1280, 197 px at 360 with every counter shown; with the C24 cockpit counters: 163 px at 1280, 215 px at 360 |
| Gauges without capacity | tool calls and subagents become dials with no scale: decorative | they stay counters; only quantities with a capacity get a rule |
| Owner's word "gauge" | literal | linear gauges, not dials |
| Risk | the AI-dashboard default (row of rings) | the rule must keep its legend legible at 360 |
| Cost per tick | 5 arcs redrawn | 3 rules; same `queue_draw`-on-change rule |

**Recommendation: B, chosen by the owner (section 8).** It answers both questions of the scene; A answers only
the first and spends the top of a 360 px window on rings. If the owner wants
dials anyway, A is buildable with the same tokens and the same gauge anatomy
(section 4), and C17 changes; C18 does not.

## 4. Gauge anatomy (C17)

A gauge is drawn only where there is a capacity. The owner's brief reclaims
the gauge that the quality floor refuses by default, under one condition: it
is never decorative.

```
 label   rule (track · agents share · rest of machine)   readout · capacity
 RAM     ██████████████▒▒▒▒▒▒▒░░░░░░░░░░░░               29.3 of 38.0 GiB
                                                          agents 20.1 GiB
 tooltip / accessible label: "RAM 29.3 of 38.0 GiB used, agents 20.1 GiB
 (PSS of session processes). Source: /proc/meminfo, smaps_rollup."
```

- **Value + capacity + source**, always. The readout says the capacity in
  words ("of 16 cores", "of 38.0 GiB"); the tooltip and the accessible label
  carry the source.
- Absent value: the rule shows the empty track and the readout a dash, with
  the tooltip "no source for this agent yet". Never 0.
- Segments are separated by a 2 px gap; each fill is >= 3:1 against the track
  (`--ac-mark-*`, measured). Amber vs slate differ in hue, and the readout
  carries the exact numbers, so colour is never the only channel.
- Over a limit (PSI >= 10 %): the readout turns crimson and a 2 px tick marks
  the limit on the rule.

| Reading | Capacity | Agents segment | Source |
|---|---|---|---|
| CPU | 100 % of N cores | `summary.sessions_cpu_pct` / cores | `/proc/stat`, per-process `stat` |
| RAM | `mem_total` | `summary.sessions_pss_bytes` | `meminfo`, `smaps_rollup` |
| Swap | `swap_total` | `summary.sessions_swap_pss_bytes` | `meminfo`, `smaps_rollup` |
| Parallel activity | cores (may overflow: the rule fills and the readout says "76 of 16 cores") | `working_child_processes` | per-process `stat` (state R/D, CPU >= 1 %) |
| Memory / I/O pressure | 100 % of 10 s, limit tick at 10 % | — | `/proc/pressure` |
| Sessions by agent, by state | the session total (100 % stacked) | — | adapters, overlay status |
| Tool calls in flight, per minute; subagents | **none: counters** | — | transcripts (C14/C15) |

## 5. Rich session row (C18)

Two lines, >= 56 px, grouped under "Blocked · n", "Busy · n", "Idle · n",
"Unknown · n"; herdr multiplexers in their own group at the end.

```
[icon] name or herdr title (ellipsis at the end)     [pill: Busy]  2 tools · 1 sub
       agent · ~/short/cwd (ellipsis at the start) · w2/p27
       CPU ▮▮▯ 16%     728 MiB + 174 MiB swap     ctx ▮▮▮▮▯ 86%     I/O-bound
```

- Identity first: symbolic icon per agent type, name, then agent, path and
  pane in secondary text, monospace only for the path.
- State pill: amber fill for busy, crimson for blocked, outline for idle and
  unknown. Text on the pill is measured (`--ac-on-busy-text`,
  `--ac-on-blocked-text`).
- Cost: thin rules for CPU (of one core per process, capped) and context %,
  memory as PSS + swap in text.
- Activity: tool calls in flight and live subagents; a dash with a tooltip
  when the source is missing.
- Focus ring on the row from the keyboard, arrows move, Enter selects.

## 6. Tokens

`crates/agentcenter/resources/tokens.css`, loaded with `summary.css` and
`sessions.css` in one `GtkCssProvider` by `app.rs`:

- **Primitive** `--ac-p-*`: raw values, never read by a widget rule.
- **Semantic** `--ac-surface-*`, `--ac-text-*`, `--ac-mark-*`, `--ac-track`,
  `--ac-busy`, `--ac-blocked`, `--ac-selection` and the `--ac-on-*` texts.
  The dark theme remaps only this layer.
- **libadwaita** variables (`--window-bg-color`, `--accent-bg-color`,
  `--warning-color`, `--dim-opacity`, ...) point at the semantic layer, so
  stock widgets wear the palette.
- **Component** rules live in `summary.css` (C17) and `sessions.css` (C18).

Since GTK 4.20 every CSS provider has its own `prefers-color-scheme`; libadwaita
only sets its own. `app.rs` binds ours to `AdwStyleManager` (`dark` and
`high-contrast`), so the `@media (prefers-color-scheme: dark)` block follows
the system and `ADW_DEBUG_COLOR_SCHEME`.

`--dim-opacity` goes from libadwaita's 55 % (3.86:1 on the light window) to
66 % (5.49:1 or more), and the focus ring is the solid `--ac-mark-focus` instead
of the accent at 50 %.

`python3 -I scripts/contrast.py` measures every pair by token name in both
themes: 86 pairs, lowest text 5.49:1 (dimmed text on the light window), lowest
mark 3.71:1 (slate on the dark track).

## 7. Preview and tooling

The preview applies only the tokens and CSS to the C10 window (no widget
changed): the instrument panel, amber rules, crimson pressure, tabular
numerals, 40 px rows with a hairline, secondary column headers and paths. It
is a preview of the palette and the type, not of layout B: the table stays
until C18.

- `AGENTCENTER_SNAPSHOT_JSON=<file>`: the GUI publishes that `Snapshot` every
  tick without the sampler (`AGENTCENTER_SNAPSHOT_INTERVAL_MS`, default 1000).
  A file that does not parse exits 2.
- Fixtures in `crates/agentcenter/tests/fixtures/snapshots/`, generated by
  `gen.py` with a fixed seed: `tipico-40` (38 agents + 2 herdr, 9 busy,
  3 blocked, memory pressure over the limit), `nomes-longos` (long, accented,
  CJK and bidi names), `vazio`, `uma` (D-state fallback, banner shown). They
  are schema 4 and carry C13's `summary`, computed from their own sessions by
  the rule of `Summary::of`. `snapshot_fixtures_follow_the_core_summary_and_schema`
  (`feed.rs`) fails when a fixture's `summary` or overlay keys, its
  `schema_version` or its numbers drift from the core: `#[serde(default)]`
  would otherwise read a missing or invented field as zero, silently.
- `scripts/capture-gui.sh`: 4 fixtures x 1280x800, 840x700, 360x640 x light,
  dark = 24 PNG in `target/screenshots/`.
- `scripts/measure-gui.sh --threads`: CPU per thread, with the GTK main loop
  on its own line.

## 8. Decisions for the owner

Decided by the owner on 17/10/2026:

1. **Layout B.** The machine rule (CPU, RAM, swap split agents / rest / free)
   over the session rack grouped by state, blocked first. C17 builds the rule,
   C18 the rack. A is not built.
2. **Fixed amber accent.** Amber is the colour of work in progress and does
   not follow the system accent; crimson is "needs you" (blocked, pressure).
3. **A cockpit recommendation is a pill on the row, nothing more.** It does
   not tint the whole row: the row keeps its state colour and the
   recommendation reads as one more pill beside the state pill. Tokens
   (C24, `tokens.css`): `--ac-text-rec-*` and `--ac-mark-rec-*` for ok
   (neutral, no success colour in the palette), compact (crimson outline),
   handoff and loop (crimson fill); `contrast.py` requires all of them in both
   themes and measures them on the selected row too. The pill shows the
   script's verdict name (COMPACTAR, OCIOSA, LOOP, lendo) so it matches
   `contexto.py`; the screen reader speaks it in English.

### CC2 stage 1 — command center (owner brief, 2026-10-19)

The unselected detail area becomes session resource history: CPU, PSS and
SwapPss strips, using the existing amber/track tokens and graph widget, not
financial tiles. The rack can use 44% of the split, up to 620 px. Below
840 sp the history moves into a native disclosure above the rack, initially
closed to preserve room for sessions; the existing detail/Back flow remains.

History is local to this window: at most 600 actual Snapshot observations,
positioned in one-second slots by taken_at_ms. No data before observation,
no interpolation, no new source reads. Missing or partial sums break the
line; the current partial sum is labeled explicitly, never called a total.
Each strip exposes current value, unit, scale, UTC observation range and
source as accessible text. There is no animation or independent timer.
This implementation checkpoint is not visual acceptance: native captures,
keyboard/Back, performance and compile checks remain pending.

### CC2 native density correction — 2026-10-20

The first native captures show a 382px narrow header band (previous cockpit
baseline: 215px) and a missing Swap rule. Header filters are desktop GTK
pointer/keyboard controls, not mobile touch controls: their GtkButtons now
use a 24px CSS minimum height, 4px horizontal padding and regular weight,
instead of a hard-coded 44×44 request. This is an explicit desktop exception
to the mobile touch-target floor. All counters and native keyboard activation
remain; hover uses the existing selection token and focus a 2px amber ring.

The rules' BreakpointBin uses a font-relative CSS minimum (6em, 5em narrow)
instead of 48px, reserving room for all three readings as system text scales.
The rack enters narrow density at 600sp instead of 520sp: the intermediate
row requested about 576px in the 563px split. Name, state, recommendation,
CPU, memory and context remain visible. No palette or semantic tokens change.

Bounded CC2 verification (2026-10-21): the current WIP passes 6 dashboard,
14 session-list and 5 Reclaim tests, an offline debug build and all 134 token
contrast pairs. Six synthetic `tipico-40` captures now have the requested
1280×800, 840×700 and 360×640 sizes in both themes, with machine bands of
176, 200 and 275px respectively. Swap is visible; capture logs contain no
AdwBreakpointBin-height or action-target warnings. Evidence lives in
`target/cc2-recapture-final`, with wide captures on Wayland and smaller ones
on isolated Broadway. Broadway's 1024px display cannot validate 1280px.

Capture windows no longer inherit the owner's maximized preference or save
capture dimensions on close. Otherwise every requested viewport inherited a
maximized window, invalidating size checks. Production window preferences
remain unchanged. Text scaling, keyboard flow and performance are not verified.
Kernel identity and terminal focus remain integration work: CC1 must expose
`Session.starttime: Option<u64>` and its focus capability/disabled reason;
overlay-ID navigation alone is not protection against kernel PID reuse.

Still open:

4. A bundled typeface of our own, or the platform's (current choice).
