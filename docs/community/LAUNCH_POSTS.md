# Launch post drafts

These are drafts only. The repository owner should adapt them to each
community's current rules and post them personally. Do not remove the pre-alpha
or failing-gates disclosure.

## Show HN

**Title:** Show HN: AgentCenter – see what each AI coding session costs your Linux machine

I built AgentCenter because a coding-agent session is rarely one process. It can
leave MCP servers, shells, builds, and other children working, while an ordinary
process list makes it hard to see which session owns the CPU, memory, or disk
activity.

AgentCenter groups the process tree by coding session and samples CPU, PSS plus
swap, read/write throughput, and I/O stall. It has a GTK4/libadwaita desktop app
and a Rust CLI that prints a table or JSON. Built-in adapters cover Claude Code,
Codex, pi, Copilot CLI, Gemini CLI, Kiro CLI, OpenCode, Aider, Goose, and Cursor
Agent.

This is Linux-only and pre-alpha. The D-state performance gate passes in CI with
almost no headroom (4.727 ms against a budget below 5 ms on commit 014a336); the
last recorded GUI first-frame and CPU measurements miss their budgets, and
session actions still need acceptance work. I would value feedback on the
measurement model, procfs edge cases, and what a useful session-level view
should show.

- Project: https://github.com/eduardopin/agentcenter
- Overview: https://eduardopin.github.io/agentcenter/
- Known limitations: https://github.com/eduardopin/agentcenter/blob/main/docs/KNOWN_LIMITATIONS.md

## r/rust

**Suggested title:** AgentCenter: a Rust + GTK4 resource monitor grouped by AI coding session (pre-alpha)

I am opening up AgentCenter, a Linux resource monitor written in Rust with a
GTK4/libadwaita UI and a headless CLI.

The core modeling choice is to treat the coding session—not an individual PID—as
the unit. The sampler walks `/proc`, identifies known agent processes, attaches
their descendants, and reports CPU deltas, proportional memory and swap, disk
rate, and I/O stall. The CLI emits the same snapshot as a table or JSON.

It is AGPL-3.0 licensed and pre-alpha. The repository documents failing performance
and GUI gates rather than calling them done: the 500-process D-state median
was 4.727 ms against a budget below 5 ms in CI on commit 014a336 (1.06x
headroom), and the last recorded GUI first-frame and CPU measurements miss
their budgets. I am especially interested in review of
the procfs code, fixtures, benchmarks, and adapter boundary.

- Code and build instructions: https://github.com/eduardopin/agentcenter
- Known limitations: https://github.com/eduardopin/agentcenter/blob/main/docs/KNOWN_LIMITATIONS.md

## r/linux

**Suggested title:** AgentCenter is a Linux monitor that groups resource use by AI coding session (pre-alpha)

AgentCenter tries to answer a narrow Linux desktop question: which coding-agent
session is using my CPU, memory, swap, or disk right now?

Instead of showing only individual processes, it groups each detected agent with
its MCP servers, shells, builds, and other descendants. The GTK4 app and CLI
show CPU, PSS plus swap, disk read/write rates, and I/O stall per session.
Monitoring reads local procfs data and does not modify the sampled processes.

This is source-only, Linux-only, and pre-alpha. The documented performance and
GUI validation gates currently do not all pass, so it is for testing and
contribution—not a stable system utility yet.

- Overview and install: https://eduardopin.github.io/agentcenter/
- Source: https://github.com/eduardopin/agentcenter

I would appreciate reports with distro, kernel, desktop, and agent versions.
Please scrub project paths, hostnames, credentials, and session identifiers.

## r/ClaudeAI

**Suggested title:** I made a Linux monitor that attributes Claude Code and other agent process trees to their sessions

Claude Code can start MCP servers, shells, or builds. AgentCenter groups that
whole process tree under the coding session and reports CPU, proportional memory
and swap, disk throughput, and I/O stall.

Claude Code is one built-in adapter alongside Codex, pi, Copilot CLI, Gemini
CLI, Kiro CLI, OpenCode, Aider, Goose, and Cursor Agent.

Honest status: Linux-only, source-only, and pre-alpha. Current documented
performance and GUI gates fail, and control features still have incomplete
acceptance evidence. Please read the limitations before trying any session
action.

- Project: https://github.com/eduardopin/agentcenter
- Known limitations: https://github.com/eduardopin/agentcenter/blob/main/docs/KNOWN_LIMITATIONS.md

## X

I built AgentCenter to show which AI coding session owns the CPU, memory, swap,
and disk activity hidden in its process tree.

Rust + GTK4 on Linux, with a table/JSON CLI. AGPL-3.0 licensed and honestly
pre-alpha: current performance and GUI gates still fail.

https://eduardopin.github.io/agentcenter/

## LinkedIn

AI coding sessions do more than run one process. They launch MCP servers,
shells, builds, and other children—so it can be difficult to tell which session
is responsible when a Linux workstation starts using CPU, memory, swap, or disk.

I built AgentCenter to make the session the unit of observation. It groups the
agent and its process tree, then reports CPU, proportional memory and swap, disk
throughput, and I/O stall. The project includes a GTK4/libadwaita desktop app
and a Rust CLI with table and JSON output.

AgentCenter is public under the AGPL-3.0 license, but it is pre-alpha—not a finished
release. The repository documents that the current full performance gate and
the latest GUI first-frame and CPU budgets fail, and that some session-action
acceptance work remains open. I am looking for contributors interested in
Linux procfs measurement, Rust, GTK accessibility, testing, agent detection,
and clear documentation.

- Overview: https://eduardopin.github.io/agentcenter/
- Source and contribution guide: https://github.com/eduardopin/agentcenter
