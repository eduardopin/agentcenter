//! CC4: Claude Code control through a fake Herdr server (protocol 22, private
//! 0700 directory, 0600 socket, one request per connection) and transcript
//! files written by the fake as Claude would. Nothing here talks to a real
//! Herdr or Claude.
use agentcenter_core::control::{
    Action, Evidence, State,
    claude::{Controller, Env, Subject, code},
    herdr::{Endpoint, Target},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SID: &str = "11111111-2222-4333-8444-555555555555";
const OTHER: &str = "99999999-2222-4333-8444-555555555555";
const OP: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OP2: &str = "bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee";
static SEQ: AtomicU64 = AtomicU64::new(0);

type Hook = Box<dyn FnMut(&str) + Send>;
struct Fake {
    sessions: Vec<&'static str>,
    gets: usize,
    status: &'static str,
    prompts: Vec<String>,
    on_prompt: Option<Hook>,
}

struct World {
    root: PathBuf,
    home: PathBuf,
    transcript: PathBuf,
    fake: Arc<Mutex<Fake>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
    endpoint: Endpoint,
    subject: Subject,
}
impl Drop for World {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.server.take() {
            h.join().unwrap();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
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

fn append(path: &Path, lines: &[Value]) {
    let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn compact_lines(sid: &str) -> Vec<Value> {
    let t = now_ms() + 1;
    vec![
        json!({"type":"system","subtype":"compact_boundary","sessionId":sid,"timestamp":iso(t + 5),
            "compactMetadata":{"trigger":"manual","preTokens":100}}),
        json!({"type":"user","sessionId":sid,"timestamp":iso(t),
            "message":{"content":"<command-name>/compact</command-name>\n<command-args></command-args>"}}),
    ]
}

fn handoff_lines(op: &str, file: &Path) -> Vec<Value> {
    let t = now_ms() + 1;
    vec![
        json!({"type":"user","sessionId":SID,"timestamp":iso(t),"message":{"content":
            format!("<command-name>/clear-pickup</command-name><command-args>handoff agentcenter-op:{op}</command-args>")}}),
        json!({"type":"assistant","sessionId":SID,"timestamp":iso(t + 1),"message":{"content":[
            {"type":"tool_use","id":format!("toolu_{op}"),"name":"Write","input":{"file_path":file,"content":"x"}}]}}),
        json!({"type":"user","sessionId":SID,"timestamp":iso(t + 2),
            "message":{"content":[{"type":"tool_result","tool_use_id":format!("toolu_{op}"),"content":"ok"}]},
            "toolUseResult":{"type":"create","filePath":file}}),
    ]
}

fn world(sessions: Vec<&'static str>, status: &'static str) -> World {
    let root = PathBuf::from(std::env::var_os("HOME").unwrap()).join(format!(
        ".ac-claude-it-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    let cwd = root.join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(home.join(".claude/handoffs")).unwrap();
    fs::create_dir_all(home.join(".claude/squad/trocas")).unwrap();
    let slug: String = cwd
        .to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let projects = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&projects).unwrap();
    let transcript = projects.join(format!("{SID}.jsonl"));
    // Prior history: an earlier manual compaction must not count.
    fs::write(&transcript, "").unwrap();
    let old = now_ms() - 60_000;
    append(
        &transcript,
        &[
            json!({"type":"system","subtype":"compact_boundary","sessionId":SID,
            "timestamp":iso(old),"compactMetadata":{"trigger":"manual"}}),
        ],
    );
    let listener = UnixListener::bind(root.join("s")).unwrap();
    fs::set_permissions(root.join("s"), fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let pid = std::process::id();
    let fake = Arc::new(Mutex::new(Fake {
        sessions,
        gets: 0,
        status,
        prompts: Vec::new(),
        on_prompt: None,
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let (f, st) = (fake.clone(), stop.clone());
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
            let mut f = f.lock().unwrap();
            let agent = |sid: &str, status: &str| {
                json!({"agent":"claude","pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1",
                    "terminal_id":"t","focused":false,"revision":1,"agent_status":status,
                    "agent_session":{"source":"hook","agent":"claude","kind":"id","value":sid}})
            };
            let result = match q["method"].as_str().unwrap() {
                "agent.list" => json!({"type":"agent_list","agents":[agent(SID, f.status)]}),
                "pane.process_info" => json!({"type":"pane_process_info","process_info":
                    {"pane_id":"w1:p1","foreground_processes":[{"pid":pid,"name":"claude"}]}}),
                "agent.get" => {
                    assert_eq!(q["params"], json!({"target":"w1:p1"}));
                    let i = f.gets.min(f.sessions.len() - 1);
                    f.gets += 1;
                    json!({"type":"agent_info","agent":agent(f.sessions[i], f.status)})
                }
                "agent.prompt" => {
                    assert_eq!(q["params"]["target"], "w1:p1");
                    let text = q["params"]["text"].as_str().unwrap().to_string();
                    if let Some(h) = f.on_prompt.as_mut() {
                        h(&text);
                    }
                    f.prompts.push(text);
                    json!({"type":"agent_prompted","agent":agent(SID, "working")})
                }
                m => panic!("unexpected method {m}"),
            };
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
        transcript,
        fake,
        stop,
        server: Some(server),
    }
}

impl World {
    fn controller(&self) -> Controller {
        let mut c = Controller::new(self.endpoint.clone(), Env::new(self.home.clone()));
        c.io_timeout = Duration::from_secs(2);
        c
    }
    fn prompts(&self) -> Vec<String> {
        self.fake.lock().unwrap().prompts.clone()
    }
    fn on_prompt(&self, h: Hook) {
        self.fake.lock().unwrap().on_prompt = Some(h);
    }
}

#[test]
fn compact_completes_only_from_manual_boundary_evidence() {
    let w = world(vec![SID], "idle");
    let t = w.transcript.clone();
    w.on_prompt(Box::new(move |_| append(&t, &compact_lines(SID))));
    let mut c = w.controller();
    let caps = c.capabilities(&w.subject);
    assert!(caps.compact.enabled && caps.handoff.enabled, "{caps:?}");
    let o = c.start(OP, Action::Compact, &w.subject);
    assert_eq!((o.state, o.code), (State::Completed, code::COMPACTED));
    assert!(matches!(o.evidence, Some(Evidence::Compacted { .. })));
    assert_eq!(w.prompts(), ["/compact"]);
    // Clique duplo: same operation ID never resends.
    let again = c.start(OP, Action::Compact, &w.subject);
    assert_eq!((again.state, w.prompts().len()), (State::Completed, 1));
    // Telemetry: action, code, duration, opaque correlation; no text or IDs.
    let tele = serde_json::to_string(&c.telemetry().collect::<Vec<_>>()).unwrap();
    for secret in [SID, OP, "/compact", w.transcript.to_str().unwrap()] {
        assert!(!tele.contains(secret), "{tele}");
    }
    assert!(tele.contains("\"compacted\""), "{tele}");
}

#[test]
fn handoff_completes_with_verified_artifact_and_marked_prompt() {
    let w = world(vec![SID], "idle");
    let (t, file) = (w.transcript.clone(), w.home.join(".claude/handoffs/h.md"));
    w.on_prompt(Box::new(move |text| {
        let op = text.rsplit(':').next().unwrap().to_string();
        fs::write(&file, b"handoff body").unwrap();
        append(&t, &handoff_lines(&op, &file));
    }));
    let mut c = w.controller();
    let o = c.start(OP, Action::Handoff, &w.subject);
    assert_eq!((o.state, o.code), (State::Completed, code::PERSISTED));
    let Some(Evidence::Persisted(a)) = o.evidence else {
        panic!("{o:?}");
    };
    assert_eq!((a.size, a.sha256.len()), (12, 64));
    assert_eq!(
        w.prompts(),
        [format!("/clear-pickup handoff agentcenter-op:{OP}")]
    );
}

#[test]
fn accepted_send_without_evidence_is_unknown_and_late_evidence_reconciles_only_its_own() {
    let w = world(vec![SID], "idle");
    let mut c = w.controller();
    // Non-zero and hold == evidence_timeout, as in production: the hold must
    // count from when the operation became unknown, not from the send.
    let t = Duration::from_millis(100);
    (c.evidence_timeout, c.hold) = (t, t);
    let o = c.start(OP, Action::Handoff, &w.subject);
    assert_eq!((o.state, o.code), (State::Running, code::SENT));
    std::thread::sleep(t + Duration::from_millis(20));
    let o = c.status(OP).unwrap();
    assert_eq!((o.state, o.code), (State::Unknown, code::TIMEOUT));
    // Unknown still holds the session: no second operation, no second send.
    let o2 = c.start(OP2, Action::Compact, &w.subject);
    assert_eq!((o2.state, o2.code), (State::Rejected, code::PENDING));
    assert_eq!(
        c.capabilities(&w.subject)
            .compact
            .disabled_reason
            .as_deref(),
        Some(code::PENDING)
    );
    // Late evidence of another operation does not reconcile this one.
    let file = w.home.join(".claude/handoffs/h.md");
    fs::write(&file, b"x").unwrap();
    append(&w.transcript, &handoff_lines(OP2, &file));
    append(&w.transcript, &compact_lines(SID));
    assert_eq!(c.status(OP).unwrap().state, State::Unknown);
    append(&w.transcript, &handoff_lines(OP, &file));
    let o = c.status(OP).unwrap();
    assert_eq!((o.state, o.code), (State::Completed, code::PERSISTED));
    assert_eq!(w.prompts().len(), 1);
    assert_eq!(c.status(OP2).unwrap_err(), code::UNKNOWN_OPERATION);
}

#[test]
fn unknown_by_timeout_holds_the_session_from_when_it_became_unknown() {
    let w = world(vec![SID], "idle");
    let mut c = w.controller();
    (c.evidence_timeout, c.hold) = (Duration::from_millis(50), Duration::from_millis(200));
    let o = c.start(OP, Action::Compact, &w.subject);
    assert_eq!((o.state, o.code), (State::Running, code::SENT));
    // Past the evidence timeout and also past `hold` counted from the send.
    std::thread::sleep(Duration::from_millis(250));
    let o = c.status(OP).unwrap();
    assert_eq!((o.state, o.code), (State::Unknown, code::TIMEOUT));
    let o2 = c.start(OP2, Action::Handoff, &w.subject);
    assert_eq!((o2.state, o2.code), (State::Rejected, code::PENDING));
    let caps = c.capabilities(&w.subject);
    assert_eq!(caps.handoff.disabled_reason.as_deref(), Some(code::PENDING));
    assert_eq!(w.prompts().len(), 1, "second operation sent while held");
    // Once the hold has elapsed since `unknown`, a new operation may go out.
    std::thread::sleep(Duration::from_millis(250));
    let o2 = c.start(OP2, Action::Handoff, &w.subject);
    assert_eq!((o2.state, o2.code), (State::Running, code::SENT));
    assert_eq!(w.prompts().len(), 2);
}

#[test]
fn unreadable_claim_directory_fails_closed_without_send() {
    let w = world(vec![SID], "idle");
    if Env::new(w.home.clone()).uid == 0 {
        eprintln!("skipped: root ignores mode 000, the claim stays readable");
        return;
    }
    let trocas = w.home.join(".claude/squad/trocas");
    fs::write(trocas.join(format!("{SID}.claim")), b"").unwrap();
    fs::set_permissions(&trocas, fs::Permissions::from_mode(0o000)).unwrap();
    let mut c = w.controller();
    let caps = c.capabilities(&w.subject);
    let outs: Vec<_> = [(OP, Action::Compact), (OP2, Action::Handoff)]
        .into_iter()
        .map(|(id, a)| c.start(id, a, &w.subject))
        .collect();
    fs::set_permissions(&trocas, fs::Permissions::from_mode(0o700)).unwrap();
    for cap in [&caps.compact, &caps.handoff] {
        assert!(!cap.enabled, "{caps:?}");
        assert_eq!(cap.disabled_reason.as_deref(), Some(code::CLAIM_UNREADABLE));
    }
    for o in outs {
        assert_eq!((o.state, o.code), (State::Rejected, code::CLAIM_UNREADABLE));
    }
    assert!(w.prompts().is_empty(), "sent despite an unreadable claim");
}

#[test]
fn second_operation_and_mismatched_duplicate_are_rejected_without_send() {
    let w = world(vec![SID], "idle");
    let mut c = w.controller();
    assert_eq!(
        c.start(OP, Action::Compact, &w.subject).state,
        State::Running
    );
    let o = c.start(OP2, Action::Handoff, &w.subject);
    assert_eq!((o.state, o.code), (State::Rejected, code::PENDING));
    let o = c.start(OP, Action::Handoff, &w.subject);
    assert_eq!(
        (o.state, o.code),
        (State::Rejected, code::DUPLICATE_MISMATCH)
    );
    let o = c.start("not-a-uuid", Action::Handoff, &w.subject);
    assert_eq!((o.state, o.code), (State::Rejected, code::INVALID_REQUEST));
    assert_eq!(w.prompts().len(), 1);
}

#[test]
fn identity_session_busy_claim_and_socket_failures_never_send() {
    type Setup = fn(&mut World);
    let cases: Vec<(&str, Vec<&'static str>, &'static str, Setup, &str)> = vec![
        (
            "starttime differs",
            vec![SID],
            "idle",
            |w| w.subject.target.starttime += 1,
            code::IDENTITY,
        ),
        (
            "pid reused by a process outside the pane",
            vec![SID],
            "idle",
            |w| {
                // A live process of the same UID that the pane does not hold.
                let other = std::os::unix::process::parent_id();
                w.subject.target.pid = other;
                w.subject.target.starttime = starttime(other);
            },
            code::STALE,
        ),
        (
            "pane now hosts another session",
            vec![OTHER],
            "idle",
            |_| {},
            code::SESSION_MISMATCH,
        ),
        (
            "session changed between check and send",
            vec![SID, OTHER],
            "idle",
            |_| {},
            code::SESSION_MISMATCH,
        ),
        ("agent busy", vec![SID], "working", |_| {}, code::BUSY),
        (
            "vigia swap claimed",
            vec![SID],
            "idle",
            |w| {
                fs::write(
                    w.home.join(format!(".claude/squad/trocas/{SID}.claim")),
                    b"",
                )
                .unwrap()
            },
            code::VIGIA_SWAP,
        ),
        (
            "socket mode 0660",
            vec![SID],
            "idle",
            |w| fs::set_permissions(w.root.join("s"), fs::Permissions::from_mode(0o660)).unwrap(),
            code::UNSAFE_SOCKET,
        ),
        (
            "socket directory group-writable",
            vec![SID],
            "idle",
            |w| fs::set_permissions(&w.root, fs::Permissions::from_mode(0o770)).unwrap(),
            code::UNSAFE_SOCKET,
        ),
        (
            "transcript missing",
            vec![SID],
            "idle",
            |w| fs::remove_file(&w.transcript).unwrap(),
            code::NO_TRANSCRIPT,
        ),
    ];
    for (why, sessions, status, setup, expected) in cases {
        for action in [Action::Compact, Action::Handoff] {
            let mut w = world(sessions.clone(), status);
            setup(&mut w);
            let mut c = w.controller();
            let o = c.start(OP, action, &w.subject);
            assert_eq!((o.state, o.code), (State::Rejected, expected), "{why}");
            assert!(w.prompts().is_empty(), "{why}: sent");
            assert!(o.evidence.is_none(), "{why}");
        }
    }
}
