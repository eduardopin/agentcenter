# Changelog

All notable changes to AgentCenter are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
aims to follow [Semantic Versioning](https://semver.org/) once it leaves
pre-alpha.

## [Unreleased]

### Added

- Session list with one line per session: a state glyph, a metric bar that
  follows the sort, an Action column with Handoff and Compact buttons, a token
  battery (share of the last 30 minutes of spend and tokens per minute) and a
  context percentage.
- CPU and Memory toggle for the session list, with flat orders, headings and a
  sort menu. The list opens on CPU with a scale axis and reorders by 30-minute
  spend at most every 30 seconds.
- Token spend tracking per session (tokens since start, last 30 minutes) and a
  30-minute token sparkline in each row.
- Memory map: a treemap of sessions and processes with a token heat palette,
  pinned larger tiles, a legend and a layout that only moves every 30 seconds
  and never under the pointer or keyboard focus.
- Machine strip with charts, a RAM bar gauge, a context gauge for the fullest
  session and a gauge of open sessions with a sub-session and agent breakdown.
- Session detail page with a one-row header, four 10-minute timelines, hatched
  process bars, where-it-runs rows, Suspend, Resume and End buttons, and
  Handoff and Compact in the page header.
- Reclaim page on the memory map: pick sessions to end and act on them from
  an "End N" action bar.
- File, View and Help as header buttons, a theme switch and an About dialog
  showing the build stamp.
- Session names and ids in the detail panel; names skip slash commands and
  filler words.
- Idle state in blue and two-level machine charts with a gradient area.

### Changed

- The interface was redesigned: the dashboard, machine header and counter
  filter are gone, the session list sits on the left and the maps take the
  rest of the window.
- Every transcript is read whole on its first reading; the per-tick read
  budget applies afterwards and renews at an explicit tick start.
- The window fits 840 px wide, and the Reclaim page keeps its list and action
  bar in view at 360 px.
- The About build stamp follows HEAD and `SOURCE_DATE_EPOCH`, and the copyright
  line matches the LICENSE file.

### Fixed

- Handoff and Compact buttons now show on finished panes, since a Herdr
  "done" counts as idle.
- A list Action waits for its own session's control instead of failing at
  once or pressing the previous session's control.
- Permission prompts no longer stay open past a ceiling counted from the send,
  and an unknown prompt is never revived.
- Claude handoff evidence is no longer rejected when the file's coarse
  modification time trails the send time.
- Pickup evidence needs the exact skill command, not a substring.
- Wide windows no longer push an empty detail page at start.
- Row sparklines draw complete minutes only.
- Memory map tiles that merge stay focusable and open their largest session.
- Process rows follow their pid, and labels are written only when they change.
- Back navigation from the detail with the split open, and the sort order
  kept in captures.
- Unknown readings stay uncoloured instead of showing a value.
