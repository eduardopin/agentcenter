# Contributing to AgentCenter

Thanks for helping. AgentCenter is a resource monitor whose unit is the AI
coding session; [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains the
model, the performance budget and the decisions behind it. Read it before a
large change.

Contributions are accepted under the project's MIT license (see
[License](#license)). Third-party licenses and notices must be preserved.

## Build and test

You need Rust from [rustup](https://rustup.rs) (stable, at least the
`rust-version` in `Cargo.toml`) and Linux: the sampler reads `/proc`. The
workspace has three crates: `crates/agentcenter-core` (sampler, session model,
adapters, overlay contract), `crates/agentcenter-cli` (the headless front end)
and `crates/agentcenter` (the GTK desktop app).

```sh
cargo build --workspace
cargo test --workspace
cargo run -p agentcenter-cli -- snapshot
```

Before you send a change, run the same gate CI runs:

```sh
bash scripts/ci.sh
```

Native GTK tests require a display; they must not be skipped on headless CI.
`scripts/ci.sh` preserves a configured `DISPLAY`, `WAYLAND_DISPLAY` or
`BROADWAY_DISPLAY`. Without one, it requires `gtk4-broadwayd` (the GTK stack's
`libgtk-4-bin` package) and starts its own Broadway display, exporting
`GDK_BACKEND=broadway` and a PID-unique `BROADWAY_DISPLAY` before `cargo test`.
Its HTTP listener binds to `127.0.0.1` on an ephemeral port. The script stops
only its own backend on exit, including test failures and INT/TERM; the same
cleanup also removes the benchmark freshness stamp. It installs nothing.
For a direct `cargo test --workspace`, provide a working display yourself;
use `bash scripts/ci.sh` for automatic headless setup.

`scripts/ci.sh` is the only definition of CI; `.github/workflows/ci.yml` just
installs the toolchain and calls it. It runs, in order:

1. `scripts/test-install.sh`: `install.sh` into a throwaway prefix under
   `target/` with stub binaries (`--no-build`, no network, no `sudo`), run
   twice, validated with `desktop-file-validate` and `glib-compile-schemas
   --strict`, then `--uninstall` must leave the prefix empty. `shellcheck`
   runs on the installer scripts first when it is installed.
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo test --workspace`
5. `cargo bench -p agentcenter-core --bench tick -- tick_fixture_500`,
   then `scripts/bench-gate.sh`

Claude Code control (CC4) is exercised against a fake Herdr server on a
private socket under `$HOME` (`/tmp` ancestors are refused as world-writable)
and transcripts written by the test, never a real Herdr or Claude session:

```sh
cargo test -p agentcenter-core --test control_claude
cargo test -p agentcenter-core --lib control::
```

An end-to-end run with the real `claude` binary in an isolated Herdr (temporary
socket and `HOME`, `ANTHROPIC_BASE_URL` pointing at a local stub) is not part
of the suite yet.

To install your build for daily use, see [Install](README.md#install):
`bash install.sh` from the checkout.

### Performance gate

The architecture budgets one sampler tick over 500 processes at under 5 ms.
`crates/agentcenter-core/benches/tick.rs` measures that tick on two synthetic
500-process fixtures, one per I/O stall path: `tick_fixture_500_delayacct`
(kernel delay accounting on) and `tick_fixture_500_d_state` (off, so every
session thread is read). `scripts/bench-gate.sh` reads the median criterion
recorded for every benchmark whose id starts with `BENCH_GATE_ID` and fails if
any median is **>= 5 ms**. It also fails if no fresh result was found, so a
renamed or skipped benchmark cannot pass.

The default `BENCH_GATE_ID` is `tick_fixture_500`, so the gate covers both
stall paths, `tick_fixture_500_delayacct` and `tick_fixture_500_d_state`, at
the same 5 ms limit. The prefix also matches `tick_fixture_500_claude_40` (the
same tick plus the Claude overlay), which is held to the same limit. To look
at one path only, narrow the prefix:

```sh
BENCH_GATE_ID=tick_fixture_500_d_state bash scripts/ci.sh
```

The `D`-state path is the tight one: about 4.2 ms locally (C6b, under load),
so it has ~1.2x headroom and the gate warns about it.

Margin for shared CI runners: the limit is the budget itself, with no slack
added for CI. The slack belongs on the measured side: on your machine the
median should sit at least 2x under the limit, because a shared
`ubuntu-latest` runner runs the same tick slower and with noisy neighbours.
The gate prints the headroom of each benchmark and warns below 2x. The 2x
figure is an assumption until CI has run on GitHub for a while; revisit it
with real runner numbers. The median, not the mean, is gated because it
resists samples delayed by a neighbour. If CI goes red on the runner only,
make the tick faster; do not raise the limit.

| Variable | Default | Meaning |
|---|---|---|
| `BENCH_GATE_MS` | `5` | fail when a median is >= this many ms |
| `BENCH_GATE_ID` | `tick_fixture_500` | id prefix benchmarked and gated |
| `BENCH_GATE_HEADROOM` | `2` | warn (do not fail) below this headroom |

To check that the gate can fail, force the limit under the measured median:

```sh
BENCH_GATE_MS=0.001 bash scripts/ci.sh   # must exit non-zero
```

I/O stall comes from the per-task delay accounting of the kernel. Without
`kernel.task_delayacct=1` the sampler falls back to counting threads in `D`
state, the slower path above. To enable it until the next boot:
`sudo sysctl kernel.task_delayacct=1`.

## Writing an adapter

An adapter tells the sampler that a process is an agent. Adding one is a new
file, two lines in the adapter module and one test fixture. The built-in
adapters (`claude.rs`, `codex.rs`, `pi.rs`, `herdr.rs`) are working examples.

The trait, from `crates/agentcenter-core/src/adapters/mod.rs`:

```rust
pub trait Adapter: Send + Sync {
    fn id(&self) -> &str;
    fn role(&self) -> AgentRole;
    fn matches(&self, comm: &str, exe: &mut dyn FnMut() -> Option<String>) -> bool;
}
```

`comm` is the process name from `/proc/<pid>/comm` (the kernel truncates it at
15 bytes). `exe` reads the basename of `/proc/<pid>/exe` and costs a syscall,
so call it only when `comm` is not enough. Most agents fit in a row of the
`AdapterSpec` table, which implements the trait for you:

- `comms`: exact `comm` values that identify the agent;
- `exe_names`: executable basenames, checked only when `comm` starts with one
  of them (a renamed or truncated binary, like `codex-x86_64-un`).

The example below adds [Goose](https://github.com/block/goose), a native
binary whose `comm` is `goose`.

**1. `crates/agentcenter-core/src/adapters/goose.rs`**

```rust
//! Goose: binario nativo `goose`.

use super::{AdapterSpec, AgentRole};

pub static GOOSE: AdapterSpec = AdapterSpec {
    id: "goose",
    role: AgentRole::Agent,
    comms: &["goose"],
    exe_names: &[],
};
```

**2. Register it in `crates/agentcenter-core/src/adapters/mod.rs`**

```rust
pub mod goose; // rustfmt keeps the `pub mod` list sorted
// ...
pub fn builtin() -> Vec<Box<dyn Adapter>> {
    vec![
        // ...existing adapters...
        Box::new(&goose::GOOSE),
    ]
}
```

Order matters: the first adapter that matches wins. An agent that needs a
rule the table cannot express (say, a node script whose `comm` is `node`)
implements `Adapter` on its own type instead of using `AdapterSpec`; keep
`matches` cheap, it runs for every process on every tick.

**3. A fixture in `crates/agentcenter-core/tests/fixtures/adapters/goose/`**

(`tests/fixtures/adapters/` does not exist yet: the first adapter contributed
creates it.)

A fixture is a fake `/proc`, read through `ProcRoot`. Copy the machine files
and `1/` (init) from `tests/fixtures/proc/nested/`, then add the agent and one
child:

```text
tests/fixtures/adapters/goose/
  stat                        # machine CPU counters (copied)
  sys/kernel/task_delayacct   # copied
  1/                          # init (copied)
  20/                         # the agent: copy nested/20/, then edit
    comm                      #   goose
    cmdline                   #   goose session
    stat                      #   "20 (goose) S 1 ..." (field 4 is the ppid)
    status                    #   Name: goose, PPid: 1
    statm  io  smaps_rollup  task/20/stat  cwd -> /home/dev/app
  21/                         # a child (an MCP server, a shell): ppid 20
```

Edit `comm`, `cmdline` and the name and ppid in `stat` and `status`; keep the
other files as copied. Never put real data from your machine in a fixture.

**4. A test in `crates/agentcenter-core/tests/adapters.rs`**

```rust
use agentcenter_core::adapters::{AgentRole, Registry};
use agentcenter_core::{ProcRoot, Sampler};
use std::path::Path;

#[test]
fn goose_comm_is_detected() {
    let r = Registry::default();
    assert_eq!(
        r.detect("goose", &mut || None).map(|a| a.id()),
        Some("goose")
    );
    assert!(r.detect("goosey", &mut || None).is_none());
}

#[test]
fn goose_session_owns_its_children() {
    let root =
        ProcRoot::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adapters/goose"));
    let snap = Sampler::new(Registry::default()).tick(&root).unwrap();
    let s = snap
        .sessions
        .iter()
        .find(|s| s.agent == "goose")
        .expect("goose session");
    assert_eq!(s.role, AgentRole::Agent);
    assert_eq!(s.pid, 20);
    let mut pids: Vec<u32> = s.processes.iter().map(|p| p.pid).collect();
    pids.sort();
    assert_eq!(pids, vec![20, 21]);
}
```

Before opening the change, break the adapter on purpose (change `"goose"` in
`comms`) and watch both tests fail.

For a quick local try without writing code, the CLI also accepts extra agents
by `comm`: `AGENTCENTER_EXTRA_AGENTS=goose agentcenter-cli snapshot`.

## Writing an overlay

An overlay adds context that does not come from the process tree: herdr
workspace and pane, the agent's own session id and status, model and token
counts from transcripts. It never owns processes and never changes how a
session is measured. The contract lives in
`crates/agentcenter-core/src/overlay/mod.rs`:

```rust
pub trait Overlay: Send + Sync {
    fn id(&self) -> &str;
    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay);
}
```

Every field of `SessionOverlay` is an `Option`: fill what you know, leave the
rest alone. Put the overlay in `crates/agentcenter-core/src/overlay/<name>.rs`,
declare it with `pub mod <name>;` in `overlay/mod.rs` and add it to
`overlay::registry()`; `Sampler` applies every overlay in that list after each
tick. Its fixtures go in
`crates/agentcenter-core/tests/fixtures/overlays/<name>/`. A skeleton:

```rust
//! goose: modelo e tokens a partir dos metadados da sessao.

use super::{Overlay, SessionOverlay};
use crate::session::Session;
use std::path::PathBuf;

pub struct GooseOverlay {
    /// Raiz onde o goose guarda as sessoes; o teste aponta para uma fixture.
    pub home: PathBuf,
}

impl Overlay for GooseOverlay {
    fn id(&self) -> &str {
        "goose"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        if session.agent != "goose" {
            return;
        }
        // Le so metadados (modelo, contagem de tokens, horarios) do arquivo da
        // sessao em self.home; arquivo ausente ou ilegivel deixa tudo em None.
        let _ = (&self.home, overlay);
    }
}
```

Rules for overlays:

- `enrich` runs on the sampler thread, inside the 5 ms tick budget. Do not
  block, do not spawn processes per tick, and cache what you read by file
  modification time.
- A missing, unreadable or unexpected file leaves the fields as `None`. An
  overlay never panics and never fails the tick.
- Take the directory to read as a field, so tests point it at a fixture
  instead of your home directory.

## Privacy

Sampling and overlays are read-only and local. Explicit actions are separate
from monitoring: process signals require confirmation, and the opt-in Pi control
extension runs Handoff/Compact only for a confirmed native identity/revision.
Handoff can send bounded projected text to the provider already selected in Pi;
consent must state this and possible token cost. The GUI receives only operation
metadata and never loads the saved summary. No control is automatic or installed
in an existing session. See [the control contract](extensions/pi-control/README.md).

Control changes require its offline Node suite (including the Rust-to-Node
probe), core control/handoff tests and the unchanged project CI gate. Prepare a
private CC0 runtime first and set explicit SDK paths as documented there. Use
only synthetic credentials, a fake provider and temporary sessions/sockets.
Never point a test at a live Pi session or a real Herdr pane.

Transcripts are read for **metadata only**: model, token counts, timestamps,
session ids. The content of prompts, answers, tool calls and tool results is
never stored in a field, logged, printed, cached or sent, not even truncated
and not even in debug output.

Every overlay that opens a transcript ships a canary test: its fixture
transcript carries a unique string, for example `AGENTCENTER-CANARY-7f3a9c`,
inside every content field (prompt, answer, tool input and output), and the
test asserts that the string appears nowhere in the serialized snapshot nor in
its `Debug` output:

```rust
use agentcenter_core::overlay::{self, Overlay, goose::GooseOverlay};
use agentcenter_core::{ProcRoot, Registry, Sampler};
use std::path::Path;

const CANARY: &str = "AGENTCENTER-CANARY-7f3a9c";

#[test]
fn transcript_content_never_reaches_the_snapshot() {
    // proc/: uma sessao goose; home/: transcript com CANARY em todo conteudo.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlays/goose");
    let mut snap = Sampler::new(Registry::default())
        .tick(&ProcRoot::new(dir.join("proc")))
        .unwrap();
    let overlays: Vec<Box<dyn Overlay>> = vec![Box::new(GooseOverlay {
        home: dir.join("home"),
    })];
    overlay::apply(&overlays, &mut snap.sessions);
    assert!(
        snap.sessions.iter().any(|s| s.overlay.is_some()),
        "overlay ran"
    );
    assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    assert!(!format!("{snap:?}").contains(CANARY));
}
```

The `overlay ran` assertion keeps the test honest: a canary test whose overlay
did nothing would pass for the wrong reason: `overlay::apply` leaves
`overlay: None` on a session the overlays did not fill, so the skeleton above
fails it until `enrich` reads something. No overlay reads transcripts yet
(`overlay::registry()` is empty); the first one adds the first canary test.

## Reverse engineering closed-source agents

Some agents are closed source and do not document where they keep session
state. Contributors may use **Ghidra** and **REA** for one purpose only: to
find out where such an agent keeps its session state (which files, which
format, which socket), so that an adapter or overlay can be written against
that on-disk or on-socket interface.

- Ghidra and REA are never dependencies of AgentCenter. They are not linked,
  bundled, vendored, downloaded at build time or distributed.
- No decompiled or disassembled code enters this repository, in any form:
  not in source, not in tests, not in fixtures, not in comments.
- What enters is your own description of the interface (a path, a field
  name, a file layout) and your own code that reads it.
- Respect the agent's license and terms of use. If they forbid reverse
  engineering, do not do it; open an issue instead.

## Code from other projects

- **Mission Center and Resources are GPL-3.0.** They are reference for ideas
  only (layout, what to measure, helper-process design). Never copy, port or
  translate their code into this MIT-licensed project, not even a small
  function.
- **MBrassey/agtop is MIT.** When you consult it for a change, credit it: add
  or update its entry in [NOTICE](NOTICE) and mention it in the commit
  message. Copying MIT code also requires keeping its copyright notice in the
  file it goes into; say so in the change.
- Code from any other project needs license compatibility review and a
  NOTICE entry. Preserve its copyright and license; the project's MIT
  license does not relicense third-party code. When in doubt, write it yourself.

## Commits and changes

- One logical change per commit, with `bash scripts/ci.sh` green.
- A test that the change makes pass and that fails without it.
- Do not loosen a test, a tolerance or the bench limit to make CI green; say
  why a contract changed if a test has to change.

## License

AgentCenter is licensed under the [MIT license](LICENSE), also provided as
[LICENSE-MIT](LICENSE-MIT). Contributions are accepted under MIT. Third-party
code retains its original license and notices.

Existing license grants for earlier dual-licensed revisions are unchanged.
