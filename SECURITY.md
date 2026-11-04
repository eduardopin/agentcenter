# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub: open the repository's
Security tab and choose "Report a vulnerability". That opens a private
advisory that only you and the maintainers can read. Please do not open a
public issue or pull request for a vulnerability.

Include the affected version or commit, the steps to reproduce, and what an
attacker gains. A log or a proof of concept helps. AgentCenter runs on your
machine and reads the process table and agent transcripts, so say whether the
issue exposes that data, runs code, or sends signals to processes.

When a report is confirmed, the fix and the advisory are published together,
and the reporter is credited unless they ask not to be. The project makes no
promise of a response time or a fix date.

## Supported versions

AgentCenter is pre-alpha and has no tagged release. Only the latest commit on
`main` receives fixes.

## Scope

In scope: the `agentcenter` and `agentcenter-cli` programs, `install.sh`, and
the extensions and integrations in this repository. Out of scope: the agents
AgentCenter observes, and vulnerabilities in third-party dependencies that
have an upstream advisory (report those upstream).
