# GitHub Discussions seed drafts

These are drafts for the repository owner to post after enabling Discussions.

## Welcome post

**Title:** Welcome to AgentCenter — what should a session-first monitor show you?

AgentCenter is a pre-alpha Linux resource monitor for AI coding sessions. It
groups an agent with the MCP servers, shells, builds, and other processes below
it, then reports CPU, proportional memory and swap, disk throughput, and I/O
stall for that session.

There is a GTK4/libadwaita desktop app and a headless CLI with table and JSON
output. The project is intentionally candid about its state: the current full
performance gate and documented GUI first-frame and CPU budgets fail, and some
session-action acceptance work remains open. Please read
[`docs/KNOWN_LIMITATIONS.md`](https://github.com/eduardopin/agentcenter/blob/main/docs/KNOWN_LIMITATIONS.md)
before trying it on important work.

If you test AgentCenter, tell us about the machine and agent mix—not your prompt
or transcript content. Bug reports should scrub usernames, private project
paths, hostnames, session IDs, and credentials.

- Project: https://github.com/eduardopin/agentcenter
- Install and overview: https://eduardopin.github.io/agentcenter/

## Questions for the community

1. When several coding agents are running, which session-level signal helps you
   decide what to stop or investigate first: memory, sustained CPU, disk rate,
   I/O stall, context use, or something else?
2. Which Linux distribution, desktop environment, and coding-agent combination
   should the project validate next? Please include exact versions where safe.
3. For a first contribution, would you rather work on agent detection, procfs
   measurement, the GTK interface, CLI output, accessibility evidence, tests,
   or documentation?
