//! E2E against a REAL Claude Code in a REAL, isolated Herdr: the CC4 Handoff
//! and Compact to `completed`, the Escape into a real open dialog, and the CC6 handoff-pickup to `pickup_loaded`.
//! Both run on the same pane, one after the other (`--test-threads=1`); each
//! reads the pane's current session from Herdr, so the order does not matter.
//! `#[ignore]`: they only run under `scripts/e2e-claude-herdr.sh`,
//! which builds the isolated HOME, Herdr server, API stub and agent pane and
//! exports the `AC_E2E_*` variables read here. Without them it fails, so a
//! stray `--ignored` run can never pass with nothing tested.
use agentcenter_core::control::{
    Action, Evidence, Outcome, State,
    claude::{Controller, Env, Subject, code, resume_command},
    herdr::{Endpoint, Target},
    pickup::Stage,
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const OP: &str = "e2e0c6aa-0000-4000-8000-00000000c6c6";
const HANDOFF_OP: &str = "e2e0c4aa-0000-4000-8000-00000000c4c4";
const COMPACT_OP: &str = "e2e0c4bb-0000-4000-8000-00000000c4c4";

fn var(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k} unset: run scripts/e2e-claude-herdr.sh"))
}
fn starttime(pid: u32) -> u64 {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(19).unwrap().parse().unwrap()
}
/// One raw request to the isolated Herdr, reply line as JSON (for recording
/// the real reply shapes; the controller never goes through here).
fn raw(socket: &str, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    let mut q =
        serde_json::to_vec(&serde_json::json!({"id":"e2e","method":method,"params":params}))
            .unwrap();
    q.push(b'\n');
    s.write_all(&q).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn transcript(home: &Path, sid: &str) -> Option<PathBuf> {
    fs::read_dir(home.join(".claude/projects"))
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{sid}.jsonl")))
        .find(|p| p.is_file())
}

/// `agent.get` of the pane once Herdr reports it idle (or done, which the
/// controller's gate also accepts). One action after another on one pane,
/// Claude may still be finishing the last one, and a busy pane is refused
/// (correctly). Panics after 60 s with the last status.
fn wait_idle(socket: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let got = raw(
            socket,
            "agent.get",
            serde_json::json!({"target": var("AC_E2E_PANE")}),
        );
        let status = &got["result"]["agent"]["agent_status"];
        if matches!(status.as_str(), Some("idle" | "done")) {
            return got;
        }
        assert!(
            Instant::now() < deadline,
            "pane still {status} after 60 s: {got}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Controller and subject for the pane as it is now: the session ID is read
/// live from Herdr (a pickup earlier in the run moves the pane to a new
/// session; the Claude Code process stays the same).
fn arrangement() -> (PathBuf, Controller, Subject) {
    let home = PathBuf::from(var("AC_E2E_HOME"));
    let socket = var("AC_E2E_SOCKET");
    let server_pid: u32 = var("AC_E2E_SERVER_PID").parse().unwrap();
    let pid: u32 = var("AC_E2E_PID").parse().unwrap();
    // Guard: never the owner's HOME or Herdr, only the script's ac-e2e.* root.
    let root = home.parent().expect("AC_E2E_HOME has a parent");
    let in_e2e_root = |p: &Path| {
        p.starts_with(root)
            && root
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("ac-e2e."))
    };
    assert!(in_e2e_root(&home), "{}", home.display());
    assert!(in_e2e_root(Path::new(&socket)), "{socket}");
    let got = wait_idle(&socket);
    let session = got["result"]["agent"]["agent_session"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !session.is_empty(),
        "herdr reported no agent_session_id: {got}"
    );
    println!(
        "e2e: agent.get status: {} session {session}",
        got["result"]["agent"]["agent_status"]
    );
    let endpoint = Endpoint {
        socket: socket.into(),
        server_pid,
        server_starttime: starttime(server_pid),
    };
    let subject = Subject {
        target: Target {
            pid,
            starttime: starttime(pid),
            adapter: "claude".into(),
            workspace_id: var("AC_E2E_WORKSPACE"),
            tab_id: var("AC_E2E_TAB"),
            pane_id: var("AC_E2E_PANE"),
        },
        session_id: session,
        cwd: Some(var("AC_E2E_REPO")),
    };
    let mut c = Controller::new(endpoint, Env::with_state_home(home.clone(), None));
    c.evidence_timeout = Duration::from_secs(90);
    c.idle_timeout = Duration::from_secs(60);
    c.clear_timeout = Duration::from_secs(30);
    c.load_timeout = Duration::from_secs(90);
    (home, c, subject)
}

/// Polls a CC4 operation until it leaves `running` (at most 120 s).
fn settle(c: &mut Controller, op: &str) -> Outcome {
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let o = c.status(op).unwrap();
        if o.state != State::Running || Instant::now() >= end {
            return o;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The pane's screen, as Herdr reads it.
fn screen() -> String {
    let out = std::process::Command::new("herdr")
        .args(["pane", "read", &var("AC_E2E_PANE")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// CC4/CC6 Escape path against the real thing: a real Claude Code with a real
/// permission dialog open (the stub asks for a Write into `.git/`, a protected path, so
/// the agent is `blocked`), one raw `agent.send_keys ["esc"]` (the payload
/// `herdr.rs` builds for the controller's `Clearing` path, copied here, not
/// sent through the controller), and the dialog is gone, the write never
/// happened and the agent is back at its prompt on the same session.
#[test]
#[ignore = "needs scripts/e2e-claude-herdr.sh (real Claude Code + isolated Herdr)"]
fn real_claude_escape_dismisses_an_open_permission_dialog() {
    let socket = var("AC_E2E_SOCKET");
    let pane = var("AC_E2E_PANE");
    let dialog_file = PathBuf::from(var("AC_E2E_DIALOG_PATH"));
    let before = wait_idle(&socket);
    let session = before["result"]["agent"]["agent_session"]["value"].clone();
    let sent = raw(
        &socket,
        "agent.prompt",
        serde_json::json!({"target": pane, "text": "e2e dialog"}),
    );
    assert_eq!(sent["result"]["type"], "agent_prompted", "{sent}");
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let got = raw(&socket, "agent.get", serde_json::json!({"target": pane}));
        if got["result"]["agent"]["agent_status"] == "blocked" {
            break;
        }
        assert!(Instant::now() < end, "never blocked on the dialog: {got}");
        std::thread::sleep(Duration::from_millis(200));
    }
    // Herdr reports `blocked` a moment before the dialog takes keys; the
    // controller only sends Escape after its clear timeout, never that early.
    std::thread::sleep(Duration::from_secs(2));
    assert!(screen().contains("Do you want to create"), "{}", screen());

    let esc = raw(
        &socket,
        "agent.send_keys",
        serde_json::json!({"target": pane, "keys": ["esc"]}),
    );
    println!("e2e: escape reply on a real dialog: {esc}");
    assert_eq!(esc["result"], serde_json::json!({"type": "ok"}), "{esc}");

    let after = wait_idle(&socket);
    assert_eq!(after["result"]["agent"]["agent_session"]["value"], session);
    let screen = screen();
    assert!(!screen.contains("Do you want to create"), "{screen}");
    assert!(!dialog_file.exists(), "the write was refused, not done");
}

/// CC4 E2E: Handoff and Compact on the real Claude Code, each to `completed`
/// with its evidence (persisted artifact; manual compact boundary).
#[test]
#[ignore = "needs scripts/e2e-claude-herdr.sh (real Claude Code + isolated Herdr)"]
fn real_claude_handoff_and_compact_complete() {
    let (_home, mut c, subject) = arrangement();
    let repo = PathBuf::from(var("AC_E2E_REPO"));

    let t0 = Instant::now();
    let o = c.start(HANDOFF_OP, Action::Handoff, &subject);
    println!("e2e: handoff start {:?} {}", o.state, o.code);
    let o = settle(&mut c, HANDOFF_OP);
    println!("e2e: handoff {:?} {} {:?}", o.state, o.code, o.evidence);
    println!("e2e: measure handoff_ms={}", t0.elapsed().as_millis());
    assert_eq!(
        (o.state, o.code),
        (State::Completed, code::PERSISTED),
        "{o:?}"
    );
    let Some(Evidence::Persisted(a)) = &o.evidence else {
        panic!("{o:?}")
    };
    assert_eq!(
        a.path,
        repo.join(format!(".claude/handoffs/e2e-{HANDOFF_OP}.md"))
    );
    assert_eq!(a.size, fs::metadata(&a.path).unwrap().len());

    wait_idle(&var("AC_E2E_SOCKET"));
    let t0 = Instant::now();
    let o = c.start(COMPACT_OP, Action::Compact, &subject);
    println!("e2e: compact start {:?} {}", o.state, o.code);
    let o = settle(&mut c, COMPACT_OP);
    println!("e2e: compact {:?} {} {:?}", o.state, o.code, o.evidence);
    println!("e2e: measure compact_ms={}", t0.elapsed().as_millis());
    assert_eq!(
        (o.state, o.code),
        (State::Completed, code::COMPACTED),
        "{o:?}"
    );
    assert!(
        matches!(o.evidence, Some(Evidence::Compacted { .. })),
        "{o:?}"
    );
}

#[test]
#[ignore = "needs scripts/e2e-claude-herdr.sh (real Claude Code + isolated Herdr)"]
fn real_claude_in_isolated_herdr_reaches_pickup_loaded() {
    let (home, mut c, subject) = arrangement();
    let old = subject.session_id.clone();
    // Real reply of agent.send_keys (one Escape into an idle prompt is a
    // no-op for Claude Code), recorded for the fake.
    let keys = raw(
        &var("AC_E2E_SOCKET"),
        "agent.send_keys",
        serde_json::json!({"target": var("AC_E2E_PANE"), "keys": ["esc"]}),
    );
    println!("e2e: agent.send_keys reply: {keys}");
    assert_eq!(keys["result"], serde_json::json!({"type": "ok"}));

    let mut p = c.pickup_start(OP, &subject);
    println!("e2e: start {:?} {:?} {}", p.state, p.stage, p.code);
    let end = Instant::now() + Duration::from_secs(240);
    let mut last = (p.state, p.stage, p.code);
    while p.state == State::Running && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(250));
        p = c.pickup_status(OP).unwrap();
        if (p.state, p.stage, p.code) != last {
            last = (p.state, p.stage, p.code);
            println!("e2e: {:?} {:?} {}", p.state, p.stage, p.code);
        }
    }
    let new = p.new_session_id.clone();
    // The real user entries of the slash commands, printed for the record.
    let commands = |sid: &str| -> Vec<String> {
        let Some(t) = transcript(&home, sid) else {
            return Vec::new();
        };
        fs::read_to_string(t)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["type"] == "user")
            .filter_map(|v| v["message"]["content"].as_str().map(str::to_string))
            .filter(|c| c.contains("<command-args>"))
            .collect()
    };
    for c in commands(&old) {
        println!("e2e: old session command entry: {c:?}");
    }
    let new_commands = new.as_deref().map(commands).unwrap_or_default();
    for c in &new_commands {
        println!("e2e: new session command entry: {c:?}");
    }
    assert_eq!(
        (p.state, p.stage, p.code),
        (State::Completed, Stage::PickupLoaded, code::PICKUP_LOADED),
        "{p:?}"
    );
    let new = new.unwrap();
    assert_ne!(new, old);
    // The old transcript is still on disk and readable, with the handoff turn.
    let old_file = &p.old_session.session_file;
    let text = fs::read_to_string(old_file).unwrap();
    assert!(
        text.contains(&format!("agentcenter-op:{OP}")),
        "{old_file:?}"
    );
    assert_eq!(p.old_session.session_id, old);
    assert_eq!(
        resume_command(&p).unwrap(),
        format!("claude --resume {old}")
    );
    let copy = p.handoff.as_ref().unwrap();
    assert!(
        copy.path
            .starts_with(home.join(".local/state/agentcenter/handoffs"))
    );
    // Acceptance 3: the real entry has exactly the shape of the fixtures
    // (tests/fixtures/claude_pickup/{handoff,pickup}.jsonl).
    let skill = agentcenter_core::control::claude::handoff_skill(&home);
    let shape = |args: String| {
        format!(
            "<command-message>{skill}</command-message>\n\
             <command-name>/{skill}</command-name>\n<command-args>{args}</command-args>"
        )
    };
    assert!(
        new_commands.contains(&shape(format!("pickup {}", copy.path.display()))),
        "{new_commands:?}"
    );
    assert!(
        commands(&old).contains(&shape(format!("handoff agentcenter-op:{OP}"))),
        "{:?}",
        commands(&old)
    );
    println!(
        "e2e: pickup_loaded old={old} new={new} copy={}",
        copy.path.display()
    );
}
