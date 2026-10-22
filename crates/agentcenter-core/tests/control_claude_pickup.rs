//! CC6: Claude Code handoff-pickup through a fake Herdr server (protocol 22,
//! private 0700 directory, 0600 socket, one request per connection) and the
//! transcript fixtures of `tests/fixtures/claude_pickup`. The fake answers
//! `agent.list`, `pane.process_info`, `agent.get`, `agent.prompt` and
//! `agent.send_keys` from a mutable pane state the test drives, so each stage
//! is reached and observed separately. Nothing here talks to a real Herdr or
//! Claude.
use agentcenter_core::control::{
    Action, State,
    claude::{Controller, Env, Subject, code, resume_command},
    herdr::{Endpoint, Target},
    pickup::{Progress, Stage},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SID: &str = "11111111-2222-4333-8444-555555555555";
const NEW: &str = "22222222-2222-4333-8444-555555555555";
const OTHER: &str = "99999999-2222-4333-8444-555555555555";
const OP: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OP2: &str = "bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const CANARY: &str = "AGENTCENTER-CANARY-cc6";
static SEQ: AtomicU64 = AtomicU64::new(0);

/// What the fake pane reports; prompts and keys it received.
struct Pane {
    session: &'static str,
    status: &'static str,
    agent: &'static str,
    /// Pid `pane.process_info` reports as the pane's foreground process.
    pid: u32,
    prompts: Vec<String>,
    keys: Vec<Value>,
    /// Requests whose connection is closed without a reply after the effect
    /// is recorded: an `agent.prompt` text or a method name.
    drop: Vec<String>,
    /// The pane reports no session ID.
    no_session: bool,
}
type Hook = Box<dyn FnMut(&str, &mut Pane) + Send>;

struct World {
    root: PathBuf,
    home: PathBuf,
    projects: PathBuf,
    pane: Arc<Mutex<Pane>>,
    hook: Arc<Mutex<Option<Hook>>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
    endpoint: Endpoint,
    subject: Subject,
    child: Option<Child>,
}
impl Drop for World {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.server.take() {
            h.join().unwrap();
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = chmod_all(&self.root);
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn chmod_all(dir: &Path) -> std::io::Result<()> {
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        let m = fs::symlink_metadata(&p)?;
        if m.is_dir() {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
            chmod_all(&p)?;
        } else if m.is_file() {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn starttime(pid: u32) -> u64 {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(19).unwrap().parse().unwrap()
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn iso(ms: u64) -> String {
    let (s, ms) = (ms / 1000, ms % 1000);
    let (y, mo, d) = agentcenter_core::overlay::transcript::civil_from_days((s / 86400) as i64);
    let t = s % 86400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// A fixture with its placeholders filled; timestamps start 1 ms from now.
fn fixture(name: &str, sid: &str, op: &str, path: &Path, read: &Path) -> String {
    fixture_at(now_ms() + 1, name, sid, op, path, read)
}
/// As [`fixture`], timestamps starting at `t`.
fn fixture_at(t: u64, name: &str, sid: &str, op: &str, path: &Path, read: &Path) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_pickup");
    let mut text = fs::read_to_string(dir.join(name)).unwrap();
    for n in 0..4 {
        text = text.replace(&format!("@TS{n}@"), &iso(t + n));
    }
    text.replace("@SID@", sid)
        .replace("@OP@", op)
        .replace("@ID@", &op[..8])
        .replace("@PATH@", path.to_str().unwrap())
        .replace("@READ@", read.to_str().unwrap())
}
fn append(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn world() -> World {
    let root = PathBuf::from(std::env::var_os("HOME").unwrap()).join(format!(
        ".ac-claude-pickup-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    let cwd = root.join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(cwd.join(".claude/handoffs")).unwrap();
    fs::create_dir_all(home.join(".claude/squad/trocas")).unwrap();
    let slug: String = cwd
        .to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let projects = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&projects).unwrap();
    fs::write(projects.join(format!("{SID}.jsonl")), "").unwrap();
    let listener = UnixListener::bind(root.join("s")).unwrap();
    fs::set_permissions(root.join("s"), fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let pid = std::process::id();
    let pane = Arc::new(Mutex::new(Pane {
        session: SID,
        status: "idle",
        agent: "claude",
        pid,
        prompts: Vec::new(),
        keys: Vec::new(),
        drop: Vec::new(),
        no_session: false,
    }));
    let hook: Arc<Mutex<Option<Hook>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let (p, h, st) = (pane.clone(), hook.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        while !st.load(Ordering::Relaxed) {
            let Ok((mut s, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            s.set_nonblocking(false).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut line = String::new();
            if BufReader::new(s.try_clone().unwrap())
                .read_line(&mut line)
                .is_err()
                || line.is_empty()
            {
                continue;
            }
            let q: Value = serde_json::from_str(&line).unwrap();
            let mut pane = p.lock().unwrap();
            let info = |pane: &Pane| {
                let session = if pane.no_session {
                    Value::Null
                } else {
                    json!({"source":"hook","agent":pane.agent,"kind":"id","value":pane.session})
                };
                json!({"agent":pane.agent,"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1",
                    "terminal_id":"t","focused":false,"revision":1,"agent_status":pane.status,
                    "agent_session":session})
            };
            let result = match q["method"].as_str().unwrap() {
                "agent.list" => json!({"type":"agent_list","agents":[info(&pane)]}),
                "pane.process_info" => json!({"type":"pane_process_info","process_info":
                    {"pane_id":"w1:p1","foreground_processes":[{"pid":pane.pid,"name":"claude"}]}}),
                "agent.get" => {
                    assert_eq!(q["params"], json!({"target":"w1:p1"}));
                    json!({"type":"agent_info","agent":info(&pane)})
                }
                "agent.prompt" => {
                    assert_eq!(q["params"]["target"], "w1:p1");
                    let text = q["params"]["text"].as_str().unwrap().to_string();
                    pane.prompts.push(text.clone());
                    if let Some(hook) = h.lock().unwrap().as_mut() {
                        hook(&text, &mut pane);
                    }
                    json!({"type":"agent_prompted","agent":info(&pane)})
                }
                "agent.send_keys" => {
                    assert_eq!(q["params"]["target"], "w1:p1");
                    pane.keys.push(q["params"]["keys"].clone());
                    json!({"type":"ok"})
                }
                m => panic!("unexpected method {m}"),
            };
            if pane
                .drop
                .iter()
                .any(|d| q["method"] == d.as_str() || q["params"]["text"] == d.as_str())
            {
                continue;
            }
            let mut out = serde_json::to_vec(&json!({"id":q["id"],"result":result})).unwrap();
            out.push(b'\n');
            let _ = s.write_all(&out);
        }
    });
    World {
        endpoint: Endpoint {
            socket: root.join("s"),
            server_pid: pid,
            server_starttime: starttime(pid),
        },
        subject: Subject {
            target: Target {
                pid,
                starttime: starttime(pid),
                adapter: "claude".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                pane_id: "w1:p1".into(),
            },
            session_id: SID.into(),
            cwd: Some(cwd.to_str().unwrap().into()),
        },
        root,
        home,
        projects,
        pane,
        hook,
        stop,
        server: Some(server),
        child: None,
    }
}

impl World {
    fn controller(&self) -> Controller {
        let mut c = Controller::new(
            self.endpoint.clone(),
            Env::with_state_home(self.home.clone(), None),
        );
        c.io_timeout = Duration::from_secs(2);
        c
    }
    /// As [`World::controller`], with the pickup copies under `handoffs`.
    fn controller_in(&self, handoffs: PathBuf) -> Controller {
        let mut env = Env::with_state_home(self.home.clone(), None);
        env.handoffs = handoffs;
        let mut c = Controller::new(self.endpoint.clone(), env);
        c.io_timeout = Duration::from_secs(2);
        c
    }
    fn claim(&self, sid: &str) {
        fs::write(
            self.home.join(format!(".claude/squad/trocas/{sid}.claim")),
            "",
        )
        .unwrap();
    }
    fn set(&self, f: impl FnOnce(&mut Pane)) {
        f(&mut self.pane.lock().unwrap())
    }
    fn prompts(&self) -> Vec<String> {
        self.pane.lock().unwrap().prompts.clone()
    }
    fn keys(&self) -> Vec<Value> {
        self.pane.lock().unwrap().keys.clone()
    }
    fn transcript(&self, sid: &str) -> PathBuf {
        self.projects.join(format!("{sid}.jsonl"))
    }
    fn handoff_file(&self) -> PathBuf {
        self.root
            .join("repo/.claude/handoffs/20261022-120000-cc6.md")
    }
    /// Claude as the skill would: the handoff prompt writes the file and the
    /// transcript, then the agent keeps `working` until the test frees it;
    /// `/clear` swaps the session only if `swap`; the pickup prompt reads
    /// `read` (the copy when `None`) only if `load`.
    fn claude(&self, swap: bool, load: bool, read: Option<PathBuf>) {
        let (file, old, projects) = (
            self.handoff_file(),
            self.transcript(SID),
            self.projects.clone(),
        );
        *self.hook.lock().unwrap() = Some(Box::new(move |text, pane| {
            if let Some(op) = text.strip_prefix("/clear-pickup handoff agentcenter-op:") {
                fs::write(&file, format!("{CANARY} handoff body\n")).unwrap();
                append(&old, &fixture("handoff.jsonl", SID, op, &file, &file));
                pane.status = "working";
            } else if text == "/clear" {
                if swap {
                    pane.session = NEW;
                    fs::write(projects.join(format!("{NEW}.jsonl")), "").unwrap();
                }
            } else if let Some(copy) = text.strip_prefix("/clear-pickup pickup ") {
                pane.status = "working";
                if load {
                    let copy = PathBuf::from(copy);
                    let read = read.clone().unwrap_or(copy.clone());
                    let t = projects.join(format!("{NEW}.jsonl"));
                    append(&t, &fixture("pickup.jsonl", NEW, OP, &copy, &read));
                }
            }
        }));
    }
}

/// Polls until the progress satisfies `done` or 3 s pass.
fn until(c: &mut Controller, id: &str, done: impl Fn(&Progress) -> bool) -> Progress {
    let end = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let p = c.pickup_status(id).unwrap();
        if done(&p) || std::time::Instant::now() >= end {
            return p;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn at(p: &Progress) -> (State, Stage, &'static str) {
    (p.state, p.stage, p.code)
}

#[test]
fn pickup_walks_every_stage_to_loaded_without_leaking_content() {
    let w = world();
    w.claude(false, false, None);
    let mut c = w.controller();
    let p = c.pickup_start(OP, &w.subject);
    // Handoff persisted and copied; Claude still finishing its turn.
    assert_eq!(
        at(&p),
        (State::Running, Stage::HandoffPersisted, code::COPIED)
    );
    let copy = p.handoff.clone().expect("copy artifact");
    let original = fs::read(w.handoff_file()).unwrap();
    assert_eq!(fs::read(&copy.path).unwrap(), original);
    assert_eq!(copy.size, original.len() as u64);
    assert_eq!(fs::metadata(&copy.path).unwrap().mode() & 0o777, 0o400);
    let dir = w.home.join(".local/state/agentcenter/handoffs");
    assert!(copy.path.starts_with(&dir) && copy.path != w.handoff_file());
    assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
    assert_eq!(w.prompts().len(), 1, "no /clear while working");
    // A second operation on the session is refused while the pickup runs.
    let o = c.start(OP2, Action::Compact, &w.subject);
    assert_eq!((o.state, o.code), (State::Rejected, code::PENDING));

    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(at(&p), (State::Running, Stage::Clearing, code::CLEAR_SENT));
    assert_eq!(w.prompts()[1], "/clear");

    // Herdr reports the new session in the same pane for the same process;
    // the pickup goes to it and waits for the Read.
    *w.hook.lock().unwrap() = None;
    w.set(|p| p.session = NEW);
    fs::write(w.transcript(NEW), "").unwrap();
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Running, Stage::PickupSent, code::PICKUP_SENT)
    );
    assert_eq!(p.new_session_id.as_deref(), Some(NEW));
    assert_eq!(
        w.prompts()[2],
        format!("/clear-pickup pickup {}", copy.path.display())
    );
    let mut new_session = w.subject.clone();
    new_session.session_id = NEW.into();
    let o = c.start(OP2, Action::Compact, &new_session);
    assert_eq!((o.state, o.code), (State::Rejected, code::PENDING));

    append(
        &w.transcript(NEW),
        &fixture("pickup.jsonl", NEW, OP, &copy.path, &copy.path),
    );
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Completed, Stage::PickupLoaded, code::PICKUP_LOADED)
    );
    assert_eq!(w.prompts().len(), 3);
    assert!(w.keys().is_empty());
    // The old session is still on disk, readable, and named for resume.
    assert_eq!(p.old_session.session_id, SID);
    assert_eq!(p.old_session.session_file, w.transcript(SID));
    assert!(fs::read_to_string(w.transcript(SID)).unwrap().contains(OP));
    assert!(w.handoff_file().is_file());
    assert_eq!(
        resume_command(&p).as_deref(),
        Some(format!("claude --resume {SID}").as_str())
    );
    // Same ID again: no resend.
    assert_eq!(c.pickup_start(OP, &w.subject).state, State::Completed);
    assert_eq!(w.prompts().len(), 3);
    // Only path, size, sha256, codes and IDs leave: never the bytes.
    let tele: Vec<_> = c.telemetry().cloned().collect();
    for text in [
        serde_json::to_string(&p).unwrap(),
        format!("{p:?}"),
        serde_json::to_string(&tele).unwrap(),
    ] {
        assert!(!text.contains(CANARY), "{text}");
    }
    assert!(fs::read_to_string(&copy.path).unwrap().contains(CANARY));
}

#[test]
fn accepted_sends_without_evidence_stop_unknown_at_their_stage() {
    // Handoff typed, never written.
    let w = world();
    let mut c = w.controller();
    c.evidence_timeout = Duration::from_millis(50);
    let p = c.pickup_start(OP, &w.subject);
    assert_eq!(at(&p), (State::Running, Stage::HandoffRunning, code::SENT));
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::HandoffRunning, code::TIMEOUT)
    );
    assert!(p.handoff.is_none());
    assert_eq!(w.prompts().len(), 1);

    // Pickup typed into the new session, never read; a Read of another
    // operation's copy arriving later does not count, its own does.
    let w = world();
    w.claude(true, false, None);
    w.set(|p| p.status = "idle");
    let mut c = w.controller();
    c.load_timeout = Duration::from_millis(50);
    let first = c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(at(&p), (State::Unknown, Stage::PickupSent, code::TIMEOUT));
    assert_eq!(w.prompts().len(), 3, "{first:?}");
    let copy = p.handoff.unwrap().path;
    let foreign = copy.with_file_name(format!("s00-{OP2}.md"));
    fs::write(&foreign, "x").unwrap();
    append(
        &w.transcript(NEW),
        &fixture("pickup.jsonl", NEW, OP2, &foreign, &foreign),
    );
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(at(&p), (State::Unknown, Stage::PickupSent, code::TIMEOUT));
    append(
        &w.transcript(NEW),
        &fixture("pickup.jsonl", NEW, OP, &copy, &copy),
    );
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Completed, Stage::PickupLoaded, code::PICKUP_LOADED)
    );
    assert_eq!(w.prompts().len(), 3, "nothing resent after unknown");
}

#[test]
fn session_changed_between_handoff_and_clear_never_clears() {
    let w = world();
    w.claude(true, false, None);
    let mut c = w.controller();
    let p = c.pickup_start(OP, &w.subject);
    assert_eq!(p.stage, Stage::HandoffPersisted);
    w.set(|p| (p.session, p.status) = (OTHER, "idle"));
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (
            State::Failed,
            Stage::HandoffPersisted,
            code::SESSION_MISMATCH
        )
    );
    assert_eq!(w.prompts().len(), 1);
}

#[test]
fn clear_without_session_change_sends_one_escape_and_fails_in_clearing() {
    for (status, expected, keys) in [
        ("idle", code::NOT_CLEARED, 1),
        ("working", code::NOT_CLEARED_WORKING, 0),
    ] {
        let w = world();
        w.claude(false, false, None);
        w.set(|p| p.status = "idle");
        let mut c = w.controller();
        c.clear_timeout = Duration::from_millis(50);
        let p = c.pickup_start(OP, &w.subject);
        w.set(|p| p.status = "idle");
        let p = if p.stage == Stage::HandoffPersisted {
            c.pickup_status(OP).unwrap()
        } else {
            p
        };
        assert_eq!(at(&p), (State::Running, Stage::Clearing, code::CLEAR_SENT));
        w.set(|p| p.status = status);
        let p = until(&mut c, OP, |p| p.state != State::Running);
        assert_eq!(
            at(&p),
            (State::Failed, Stage::Clearing, expected),
            "{status}"
        );
        assert_eq!(w.keys().len(), keys, "{status}");
        if keys == 1 {
            assert_eq!(w.keys()[0], json!(["esc"]));
        }
        assert_eq!(w.prompts().len(), 2, "{status}: no pickup");
        assert_eq!(p.old_session.session_id, SID);
        assert!(p.new_session_id.is_none());
        assert_eq!(w.pane.lock().unwrap().session, SID, "old session active");
    }
}

#[test]
fn tampered_copy_never_clears() {
    let w = world();
    w.claude(true, false, None);
    let mut c = w.controller();
    let p = c.pickup_start(OP, &w.subject);
    let copy = p.handoff.unwrap().path;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&copy, "edited after the check").unwrap();
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Failed, Stage::HandoffPersisted, code::COPY_MISMATCH)
    );
    assert_eq!(w.prompts().len(), 1);
}

/// After `/clear` (the pane reports the new session), the pane's process is
/// no longer the one the pickup started with; nothing more is typed.
fn after_clear(change: fn(&mut World), expected: &str) {
    let mut w = world();
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    w.subject.target.pid = pid;
    w.subject.target.starttime = starttime(pid);
    w.child = Some(child);
    w.set(|p| p.pid = pid);
    w.claude(false, false, None);
    let mut c = w.controller();
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(p.stage, Stage::Clearing, "{p:?}");
    w.set(|p| p.session = NEW);
    change(&mut w);
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(at(&p), (State::Failed, Stage::Clearing, expected));
    assert_eq!(w.prompts().len(), 2);
    assert!(p.new_session_id.is_none());
}

#[test]
fn claude_process_gone_after_clear_fails_on_identity() {
    after_clear(
        |w| {
            let mut child = w.child.take().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
        },
        code::IDENTITY,
    );
}

#[test]
fn pane_holding_another_process_after_clear_is_stale() {
    after_clear(|w| w.set(|p| p.pid = std::process::id()), code::STALE);
}

#[test]
fn pane_passed_to_another_agent_after_clear_is_stale() {
    after_clear(|w| w.set(|p| p.agent = "codex"), code::STALE);
}

#[test]
fn vigia_claim_stops_the_pickup_before_the_next_send() {
    // Before /clear: claim of the old session.
    let w = world();
    w.claude(true, false, None);
    let mut c = w.controller();
    c.pickup_start(OP, &w.subject);
    fs::write(w.home.join(format!(".claude/squad/trocas/{SID}.claim")), "").unwrap();
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Failed, Stage::HandoffPersisted, code::VIGIA_SWAP)
    );
    assert_eq!(w.prompts().len(), 1);

    // Between /clear and the pickup: the vigia claims the swap.
    let w = world();
    w.claude(false, false, None);
    let mut c = w.controller();
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    assert_eq!(c.pickup_status(OP).unwrap().stage, Stage::Clearing);
    fs::write(w.home.join(format!(".claude/squad/trocas/{SID}.claim")), "").unwrap();
    w.set(|p| p.session = NEW);
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Failed, Stage::NewSessionConfirmed, code::VIGIA_SWAP)
    );
    assert_eq!(p.new_session_id.as_deref(), Some(NEW));
    assert_eq!(w.prompts().len(), 2, "no pickup after the claim");

    // The vigia claims the new session itself.
    let w = world();
    w.claude(true, false, None);
    let mut c = w.controller();
    c.pickup_start(OP, &w.subject);
    fs::write(w.home.join(format!(".claude/squad/trocas/{NEW}.claim")), "").unwrap();
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Failed, Stage::NewSessionConfirmed, code::VIGIA_SWAP)
    );
    assert_eq!(w.prompts().len(), 2, "no pickup into a claimed session");
}

#[test]
fn read_of_another_path_is_not_a_loaded_pickup() {
    let w = world();
    w.claude(true, true, Some(w.handoff_file()));
    let mut c = w.controller();
    c.load_timeout = Duration::from_millis(50);
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(at(&p), (State::Unknown, Stage::PickupSent, code::TIMEOUT));
    assert_eq!(w.prompts().len(), 3);
}

#[test]
fn pickup_ids_are_checked_and_never_reused_across_operations() {
    let w = world();
    let mut c = w.controller();
    let p = c.pickup_start("not-a-uuid", &w.subject);
    assert_eq!(
        at(&p),
        (
            State::Rejected,
            Stage::HandoffRunning,
            code::INVALID_REQUEST
        )
    );
    assert_eq!(
        c.start(OP2, Action::Compact, &w.subject).state,
        State::Running
    );
    let p = c.pickup_start(OP2, &w.subject);
    assert_eq!(p.code, code::DUPLICATE_MISMATCH);
    // The compact in flight holds the session: the pickup does not send.
    let p = c.pickup_start(OP, &w.subject);
    assert_eq!(
        at(&p),
        (State::Rejected, Stage::HandoffRunning, code::PENDING)
    );
    let mut other = w.subject.clone();
    other.session_id = OTHER.into();
    assert_eq!(c.pickup_start(OP, &other).code, code::DUPLICATE_MISMATCH);
    assert_eq!(w.prompts().len(), 1);
    assert_eq!(c.pickup_status(OP2).unwrap_err(), code::UNKNOWN_OPERATION);
}

/// Pickup at `Clearing` with `/clear` typed and the pane still on the old
/// session; `clear_timeout` already short.
fn stuck_in_clearing(w: &World) -> Controller {
    w.claude(false, false, None);
    let mut c = w.controller();
    c.clear_timeout = Duration::from_millis(50);
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(at(&p), (State::Running, Stage::Clearing, code::CLEAR_SENT));
    c
}

#[test]
fn vigia_claim_while_clearing_sends_no_escape() {
    let w = world();
    let mut c = stuck_in_clearing(&w);
    w.claim(SID);
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(at(&p), (State::Failed, Stage::Clearing, code::VIGIA_SWAP));
    assert!(w.keys().is_empty(), "no Escape into a claimed session");
    assert_eq!(w.prompts().len(), 2);
    assert_eq!(w.pane.lock().unwrap().session, SID);
}

#[test]
fn escape_without_confirmation_is_escape_failed() {
    let w = world();
    let mut c = stuck_in_clearing(&w);
    w.set(|p| p.drop.push("agent.send_keys".into()));
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(
        at(&p),
        (State::Failed, Stage::Clearing, code::ESCAPE_FAILED)
    );
    assert_eq!(w.keys(), vec![json!(["esc"])], "one Escape, never retried");
    assert_eq!(w.prompts().len(), 2);
    assert!(p.new_session_id.is_none());
}

#[test]
fn copy_path_unfit_for_one_argument_is_refused_before_clear() {
    for bad in ["a b", "a<b", "a>b"] {
        let w = world();
        w.claude(true, true, None);
        let mut c = w.controller_in(w.home.join(format!(".local/state/{bad}/handoffs")));
        // Copied, then refused at once: the guard runs before waiting for idle.
        let p = c.pickup_start(OP, &w.subject);
        assert!(
            p.handoff
                .is_some_and(|h| h.path.to_str().unwrap().contains(bad))
        );
        w.set(|p| p.status = "idle");
        let p = c.pickup_status(OP).unwrap();
        assert_eq!(
            at(&p),
            (
                State::Failed,
                Stage::HandoffPersisted,
                code::UNSAFE_COPY_PATH
            ),
            "{bad}"
        );
        assert_eq!(w.prompts().len(), 1, "{bad}: no /clear");
        assert_eq!(w.pane.lock().unwrap().session, SID);
    }
}

#[test]
fn agent_never_idle_after_the_handoff_times_out_without_clear() {
    let w = world();
    w.claude(true, true, None);
    let mut c = w.controller();
    c.idle_timeout = Duration::from_millis(50);
    let p = c.pickup_start(OP, &w.subject);
    assert_eq!(
        at(&p),
        (State::Running, Stage::HandoffPersisted, code::COPIED)
    );
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::HandoffPersisted, code::TIMEOUT)
    );
    // Idle afterwards: still nothing typed after unknown.
    w.set(|p| p.status = "idle");
    assert_eq!(at(&c.pickup_status(OP).unwrap()), at(&p));
    assert_eq!(w.prompts().len(), 1, "no /clear");
}

#[test]
fn late_handoff_evidence_never_resumes_an_unknown_pickup() {
    let w = world();
    let mut c = w.controller();
    c.evidence_timeout = Duration::from_millis(50);
    c.pickup_start(OP, &w.subject);
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::HandoffRunning, code::TIMEOUT)
    );
    // The handoff shows up after the deadline and the agent goes idle.
    let file = w.handoff_file();
    fs::write(&file, format!("{CANARY} handoff body\n")).unwrap();
    append(
        &w.transcript(SID),
        &fixture("handoff.jsonl", SID, OP, &file, &file),
    );
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::HandoffRunning, code::TIMEOUT)
    );
    assert!(p.handoff.is_none(), "no copy after unknown");
    assert_eq!(w.prompts().len(), 1, "no /clear after unknown");
}

#[test]
fn clear_with_unknown_effect_or_no_session_stays_unknown_in_clearing() {
    // `/clear` written but not confirmed.
    let w = world();
    w.claude(false, true, None);
    w.set(|p| p.drop.push("/clear".into()));
    let mut c = w.controller();
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::Clearing, code::UNKNOWN_EFFECT)
    );
    // `/clear` took effect after all: still no pickup typed.
    w.set(|p| p.session = NEW);
    fs::write(w.transcript(NEW), "").unwrap();
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::Clearing, code::UNKNOWN_EFFECT)
    );
    assert!(p.new_session_id.is_none());
    assert_eq!(w.prompts().len(), 2);

    // The pane reports no session ID past the deadline.
    let w = world();
    w.claude(false, true, None);
    let mut c = w.controller();
    c.clear_timeout = Duration::from_millis(50);
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    assert_eq!(c.pickup_status(OP).unwrap().stage, Stage::Clearing);
    w.set(|p| p.no_session = true);
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(at(&p), (State::Unknown, Stage::Clearing, code::TIMEOUT));
    w.set(|p| (p.no_session, p.session) = (false, NEW));
    fs::write(w.transcript(NEW), "").unwrap();
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(at(&p), (State::Unknown, Stage::Clearing, code::TIMEOUT));
    assert!(w.keys().is_empty(), "no Escape without a session");
    assert_eq!(w.prompts().len(), 2);
}

#[test]
fn new_session_never_idle_times_out_before_the_pickup() {
    let w = world();
    w.claude(false, true, None);
    let mut c = w.controller();
    c.load_timeout = Duration::from_millis(50);
    c.pickup_start(OP, &w.subject);
    w.set(|p| p.status = "idle");
    assert_eq!(c.pickup_status(OP).unwrap().stage, Stage::Clearing);
    w.set(|p| (p.session, p.status) = (NEW, "working"));
    fs::write(w.transcript(NEW), "").unwrap();
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (
            State::Running,
            Stage::NewSessionConfirmed,
            code::NEW_SESSION
        )
    );
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(
        at(&p),
        (State::Unknown, Stage::NewSessionConfirmed, code::TIMEOUT)
    );
    w.set(|p| p.status = "idle");
    assert_eq!(at(&c.pickup_status(OP).unwrap()), at(&p));
    assert_eq!(w.prompts().len(), 2, "no pickup typed");
}

#[test]
fn same_pid_with_another_starttime_is_another_process() {
    let mut w = world();
    w.claude(true, true, None);
    w.subject.target.starttime += 1;
    let mut c = w.controller();
    let p = c.pickup_start(OP, &w.subject);
    assert_eq!(
        at(&p),
        (State::Rejected, Stage::HandoffRunning, code::IDENTITY)
    );
    assert!(w.prompts().is_empty());
}

#[test]
fn pickup_entries_from_before_the_send_never_count() {
    let w = world();
    w.claude(false, false, None);
    let mut c = w.controller();
    c.load_timeout = Duration::from_millis(50);
    let p = c.pickup_start(OP, &w.subject);
    let copy = p.handoff.unwrap().path;
    // The new transcript already shows this very prompt and Read, a minute
    // before anything was typed into it.
    fs::write(
        w.transcript(NEW),
        fixture_at(now_ms() - 60_000, "pickup.jsonl", NEW, OP, &copy, &copy),
    )
    .unwrap();
    w.set(|p| p.status = "idle");
    assert_eq!(c.pickup_status(OP).unwrap().stage, Stage::Clearing);
    w.set(|p| p.session = NEW);
    let p = c.pickup_status(OP).unwrap();
    assert_eq!(
        at(&p),
        (State::Running, Stage::PickupSent, code::PICKUP_SENT)
    );
    let p = until(&mut c, OP, |p| p.state != State::Running);
    assert_eq!(at(&p), (State::Unknown, Stage::PickupSent, code::TIMEOUT));
    assert_eq!(w.prompts().len(), 3);
}
