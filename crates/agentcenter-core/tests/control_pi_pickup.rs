//! CC5: Pi handoff-pickup through a fake Pi extension (protocol 3, private
//! runtime directory, rotating session and nonce) and a fake Herdr server
//! (protocol 22) that reports the Pi pane as Herdr 0.9 does: agent_session
//! `{kind:"path", value:<session file>}`. The fake Herdr runs the extension's
//! `/agentcenter-pickup` command when it is typed, so the click, the rotation
//! and the status that follows are observed end to end. Nothing here talks to
//! a real Herdr or Pi.
use agentcenter_core::control::{
    State, herdr,
    pi::{self, Action, PickupError},
    pickup::Stage,
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

const OP: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const T: Duration = Duration::from_secs(2);
static SEQ: AtomicU64 = AtomicU64::new(0);

/// The Pi process as its extension sees it, and what it received.
struct Pi {
    session_id: String,
    session_file: String,
    nonce: String,
    /// The armed operation and its record (`None` until armed).
    record: Option<Value>,
    code: &'static str,
    state: &'static str,
    requests: Vec<Value>,
}
/// What the fake Herdr pane reports, and the prompts it received.
struct Pane {
    /// Session file reported as `kind:"path"`; `None` reports no session.
    session_file: Option<String>,
    status: &'static str,
    tab_id: &'static str,
    prompts: Vec<String>,
    /// `agent.prompt` is recorded and run, then its connection is closed
    /// without a reply.
    drop_prompt: bool,
    /// `agent.prompt` is answered with a Herdr error object (after the write).
    error_prompt: bool,
}
type OnArm = Box<dyn FnMut(&mut Pane) + Send>;

struct World {
    root: PathBuf,
    runtime: PathBuf,
    dir: PathBuf,
    pi: Arc<Mutex<Pi>>,
    pane: Arc<Mutex<Pane>>,
    on_arm: Arc<Mutex<Option<OnArm>>>,
    stop: Arc<AtomicBool>,
    servers: Vec<thread::JoinHandle<()>>,
    herdr: herdr::Endpoint,
    target: herdr::Target,
}
impl Drop for World {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.servers.drain(..) {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn starttime(pid: u32) -> String {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(19).unwrap().to_string()
}
/// A base whose every ancestor the Pi client accepts (no group/world write).
fn private_base() -> PathBuf {
    let safe = |base: &PathBuf| {
        base.is_absolute()
            && base.ancestors().all(|p| {
                fs::symlink_metadata(p).is_ok_and(|m| {
                    m.is_dir() && (m.uid() == uid() || m.uid() == 0) && m.mode() & 0o022 == 0
                })
            })
    };
    ["HOME", "XDG_RUNTIME_DIR"]
        .iter()
        .filter_map(|key| std::env::var_os(key).map(PathBuf::from))
        .find(safe)
        .expect("HOME or XDG_RUNTIME_DIR must have no group/world-writable ancestor")
}
fn uid() -> u32 {
    fs::metadata("/proc/self").unwrap().uid()
}
fn private_dir(p: &Path) {
    fs::create_dir(p).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
}
fn serve(
    listener: UnixListener,
    stop: Arc<AtomicBool>,
    mut answer: impl FnMut(Value) -> Option<Value> + Send + 'static,
) -> thread::JoinHandle<()> {
    listener.set_nonblocking(true).unwrap();
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let Ok((mut s, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(1));
                continue;
            };
            s.set_nonblocking(false).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut line = String::new();
            if BufReader::new(&mut s).read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            if let Some(reply) = answer(serde_json::from_str(&line).unwrap()) {
                let _ = writeln!(s, "{reply}");
            }
        }
    })
}

fn capabilities() -> Value {
    json!({"handoff":{"enabled":true,"disabled_reason":null},"compact":{"enabled":true,"disabled_reason":null}})
}
fn expected(pi: &Pi) -> Value {
    json!({"managerId":"manager","revision":"1","sessionId":pi.session_id,
        "sessionFile":pi.session_file,"leafId":"leaf"})
}
fn metadata(pi: &Pi) -> Value {
    json!({"expected":expected(pi),"capabilities":capabilities(),
        "handoff_directory":"/private/handoffs","model_auth":"configured"})
}
fn manifest(dir: &Path, pi: &Pi) {
    let m = json!({"protocol_version":2,"session_id":pi.session_id,"session_file":pi.session_file,
        "pid":std::process::id(),"process_starttime":starttime(std::process::id()),
        "instance_nonce":pi.nonce,"socket":dir.join("s"),"expected":expected(pi),
        "capabilities":capabilities()});
    let path = dir.join("manifest.json");
    fs::write(&path, m.to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn reply(op: &str, pi: &Pi) -> Value {
    json!({"protocol_version":3,"operation_id":op,"state":pi.state,"code":pi.code,
        "pickup":pi.record,"metadata":metadata(pi)})
}

/// What the extension's `/agentcenter-pickup <op>` does once typed: persist
/// the handoff, start a child session (new ID, file and nonce), load it.
fn run_pickup(dir: &Path, pi: &mut Pi, op: &str) {
    if pi.code != "armed" || pi.record.as_ref().map(|r| &r["op"]) != Some(&json!(op)) {
        return;
    }
    let handoff = dir.join(format!("s1-0001-{op}.md"));
    fs::write(&handoff, b"synthetic handoff").unwrap();
    let old = json!({"session_id":pi.session_id,"session_file":pi.session_file});
    let parent = pi.session_file.clone();
    pi.session_id = "pi-session-2".into();
    pi.session_file = dir.join("2.jsonl").to_str().unwrap().into();
    pi.nonce = "b".repeat(64);
    manifest(dir, pi);
    pi.record = Some(json!({"op":op,"stage":"pickup_loaded","old_session":old,
        "handoff":{"path":handoff,"sha256":"ab".repeat(32),"size":17},
        "new_session":{"session_id":pi.session_id,"session_file":pi.session_file,"parent_session":parent},
        "loaded_entry_id":"entry-1"}));
    (pi.state, pi.code) = ("completed", "pickup_loaded");
}

fn world() -> World {
    let root = private_base().join(format!(
        ".ac-pi-pickup-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    private_dir(&root);
    let runtime = root.join("run");
    private_dir(&runtime);
    let dir = runtime.join("ac-1");
    private_dir(&dir);
    let pid = std::process::id();
    let pi = Arc::new(Mutex::new(Pi {
        session_id: "pi-session-1".into(),
        session_file: dir.join("1.jsonl").to_str().unwrap().into(),
        nonce: "a".repeat(64),
        record: None,
        code: "",
        state: "",
        requests: Vec::new(),
    }));
    manifest(&dir, &pi.lock().unwrap());
    let pane = Arc::new(Mutex::new(Pane {
        session_file: Some(pi.lock().unwrap().session_file.clone()),
        status: "idle",
        tab_id: "w1:t1",
        prompts: Vec::new(),
        drop_prompt: false,
        error_prompt: false,
    }));
    let on_arm: Arc<Mutex<Option<OnArm>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));

    let pi_listener = UnixListener::bind(dir.join("s")).unwrap();
    fs::set_permissions(dir.join("s"), fs::Permissions::from_mode(0o600)).unwrap();
    let (p, pn, arm) = (pi.clone(), pane.clone(), on_arm.clone());
    let pi_server = serve(pi_listener, stop.clone(), move |q| {
        let mut pi = p.lock().unwrap();
        pi.requests.push(q.clone());
        let op = q["operation_id"].as_str().unwrap().to_string();
        // The listener authenticates the live nonce and session first.
        if q["instance_nonce"] != pi.nonce.as_str() || q["session_id"] != pi.session_id.as_str() {
            return Some(json!({"protocol_version":3,"operation_id":op,
                "state":"rejected","code":"unauthorized"}));
        }
        match q["action"].as_str().unwrap() {
            "handoff_pickup" => {
                pi.record = Some(json!({"op":op,"stage":"handoff_running",
                    "old_session":{"session_id":pi.session_id,"session_file":pi.session_file}}));
                (pi.state, pi.code) = ("accepted", "armed");
                if let Some(f) = arm.lock().unwrap().as_mut() {
                    f(&mut pn.lock().unwrap());
                }
            }
            "cancel" if pi.code == "armed" => (pi.state, pi.code) = ("cancelled", "cancelled"),
            _ => {}
        }
        let mut r = reply(&op, &pi);
        if let Some(rec) = r["pickup"].as_object_mut() {
            rec.remove("op");
        }
        Some(r)
    });

    let herdr_socket = root.join("herdr.sock");
    let herdr_listener = UnixListener::bind(&herdr_socket).unwrap();
    fs::set_permissions(&herdr_socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (p, pn, d) = (pi.clone(), pane.clone(), dir.clone());
    let herdr_server = serve(herdr_listener, stop.clone(), move |q| {
        let mut pane = pn.lock().unwrap();
        let info = |pane: &Pane| {
            let session = pane.session_file.as_ref().map_or(
                Value::Null,
                |f| json!({"source":"hook","agent":"pi","kind":"path","value":f}),
            );
            json!({"agent":"pi","pane_id":"w1:p1","workspace_id":"w1","tab_id":pane.tab_id,
                "terminal_id":"t","focused":false,"revision":1,"agent_status":pane.status,
                "agent_session":session})
        };
        let result = match q["method"].as_str().unwrap() {
            "agent.list" => json!({"type":"agent_list","agents":[info(&pane)]}),
            "pane.process_info" => json!({"type":"pane_process_info","process_info":
                {"pane_id":"w1:p1","foreground_processes":[{"pid":pid,"name":"pi"}]}}),
            "agent.get" => json!({"type":"agent_info","agent":info(&pane)}),
            "agent.prompt" => {
                assert_eq!(q["params"]["target"], "w1:p1");
                let text = q["params"]["text"].as_str().unwrap().to_string();
                pane.prompts.push(text.clone());
                let mut pi = p.lock().unwrap();
                if let Some(op) = text.strip_prefix("/agentcenter-pickup ") {
                    run_pickup(&d, &mut pi, op);
                    pane.session_file = Some(pi.session_file.clone());
                }
                if pane.drop_prompt {
                    return None;
                }
                if pane.error_prompt {
                    return Some(json!({"id":q["id"],"error":{"code":"internal","message":"x"}}));
                }
                json!({"type":"agent_prompted","agent":info(&pane)})
            }
            m => panic!("unexpected method {m}"),
        };
        Some(json!({"id":q["id"],"result":result}))
    });

    let start: u64 = starttime(pid).parse().unwrap();
    World {
        herdr: herdr::Endpoint {
            socket: herdr_socket,
            server_pid: pid,
            server_starttime: start,
        },
        target: herdr::Target {
            pid,
            starttime: start,
            adapter: "pi".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p1".into(),
        },
        root,
        runtime,
        dir,
        pi,
        pane,
        on_arm,
        stop,
        servers: vec![pi_server, herdr_server],
    }
}

impl World {
    /// The Pi endpoint published right now.
    fn endpoint(&self) -> pi::Endpoint {
        let [e] = <[_; 1]>::try_from(pi::discover(&self.runtime).unwrap()).unwrap();
        e
    }
    fn click(&self, e: &pi::Endpoint) -> Result<(pi::Request, pi::Reply), PickupError> {
        pi::handoff_pickup(e, &self.herdr, Some(&self.target), OP.into(), T)
    }
    fn actions(&self) -> Vec<String> {
        let pi = self.pi.lock().unwrap();
        pi.requests
            .iter()
            .map(|q| q["action"].as_str().unwrap().to_string())
            .collect()
    }
    fn prompts(&self) -> Vec<String> {
        self.pane.lock().unwrap().prompts.clone()
    }
    fn status(&self) -> (State, Stage, &'static str) {
        let (p, _) = pi::pickup_status(
            &self.runtime,
            self.target.pid,
            &self.target.starttime.to_string(),
            OP,
            T,
        )
        .unwrap();
        (p.state, p.stage, p.code)
    }
}

#[test]
fn pi_pickup_happy_path_types_once_by_session_file_and_loads() {
    let w = world();
    let old = w.endpoint();
    let (request, armed) = w.click(&old).unwrap();
    assert_eq!(
        (armed.state, armed.code.as_str()),
        (State::Accepted, "armed")
    );
    assert_eq!(
        (request.action, request.protocol_version),
        (Action::HandoffPickup, 3)
    );
    assert_eq!(w.prompts(), [format!("/agentcenter-pickup {OP}")]);
    assert_eq!(w.actions(), ["handoff_pickup"]);
    // Completion only from status evidence, on the endpoint the process publishes now.
    let (p, e) = pi::pickup_status(
        &w.runtime,
        w.target.pid,
        &w.target.starttime.to_string(),
        OP,
        T,
    )
    .unwrap();
    assert_eq!(
        (p.state, p.stage, p.code),
        (State::Completed, Stage::PickupLoaded, "pickup_loaded")
    );
    assert_eq!(p.old_session.session_file, PathBuf::from(&old.session_file));
    assert_eq!(e.parent_session, Some(PathBuf::from(&old.session_file)));
    assert_eq!(e.new_session_file, Some(w.dir.join("2.jsonl")));
    assert_eq!(e.loaded_entry_id.as_deref(), Some("entry-1"));
    assert!(
        p.handoff
            .unwrap()
            .path
            .ends_with(format!("s1-0001-{OP}.md"))
    );
}

#[test]
fn pi_pickup_old_nonce_after_rotation_is_refused_and_never_rearms() {
    let w = world();
    let old = w.endpoint();
    let (request, _) = w.click(&old).unwrap();
    // The retained request carries the rotated-away nonce and session.
    let r = pi::exchange(&old, &request.status(), T).unwrap();
    assert_eq!(
        (r.state, r.code.as_str()),
        (State::Rejected, "unauthorized")
    );
    assert!(r.pickup.is_none());
    // A second click on the stale endpoint is refused by the extension and types nothing.
    let again = w.click(&old);
    assert!(
        matches!(&again, Err(PickupError::Refused(r)) if r.code == "unauthorized"),
        "{again:?}"
    );
    assert_eq!(w.prompts().len(), 1);
    assert_eq!(
        w.status(),
        (State::Completed, Stage::PickupLoaded, "pickup_loaded")
    );
}

#[test]
fn pi_pickup_pane_moved_between_arm_and_send_cancels_before_typing() {
    let w = world();
    *w.on_arm.lock().unwrap() = Some(Box::new(|p: &mut Pane| p.tab_id = "w1:t2"));
    let r = w.click(&w.endpoint());
    let Err(PickupError::Herdr { error, cancelled }) = r else {
        panic!("{r:?}");
    };
    assert_eq!(error, herdr::Error::Stale);
    assert_eq!(
        cancelled.map(|c| (c.state, c.code)),
        Some((State::Cancelled, "cancelled".into()))
    );
    assert!(w.prompts().is_empty());
    assert_eq!(w.actions(), ["handoff_pickup", "cancel"]);
}

#[test]
fn pi_pickup_pane_on_another_session_file_cancels_before_typing() {
    let w = world();
    let other = w.dir.join("other.jsonl").to_str().unwrap().to_string();
    *w.on_arm.lock().unwrap() = Some(Box::new(move |p: &mut Pane| {
        p.session_file = Some(other.clone())
    }));
    let r = w.click(&w.endpoint());
    assert!(
        matches!(
            &r,
            Err(PickupError::Herdr {
                error: herdr::Error::Session,
                cancelled: Some(_)
            })
        ),
        "{r:?}"
    );
    assert!(w.prompts().is_empty());
    assert_eq!(w.actions(), ["handoff_pickup", "cancel"]);
}

#[test]
fn pi_pickup_herdr_refusal_before_the_effect_cancels_the_arm() {
    let w = world();
    // Busy at the revalidation right before agent.prompt: nothing typed.
    *w.on_arm.lock().unwrap() = Some(Box::new(|p: &mut Pane| p.status = "working"));
    let r = w.click(&w.endpoint());
    assert!(
        matches!(&r, Err(PickupError::Herdr { error: herdr::Error::Busy, cancelled: Some(c) })
            if c.state == State::Cancelled),
        "{r:?}"
    );
    assert!(w.prompts().is_empty());
    assert_eq!(w.actions(), ["handoff_pickup", "cancel"]);
}

#[test]
fn pi_pickup_herdr_unknown_after_the_write_is_neither_cancelled_nor_repeated() {
    // A dropped reply and a final Herdr error after the write are the same:
    // the command may have been typed (here it was), so the arm stays (R4).
    for error_reply in [false, true] {
        let w = world();
        {
            let mut p = w.pane.lock().unwrap();
            (p.drop_prompt, p.error_prompt) = (!error_reply, error_reply);
        }
        let r = w.click(&w.endpoint());
        let Err(PickupError::Unknown(request)) = r else {
            panic!("{error_reply}: {r:?}");
        };
        assert_eq!(request.operation_id, OP);
        assert_eq!(w.prompts().len(), 1, "{error_reply}");
        assert_eq!(w.actions(), ["handoff_pickup"], "{error_reply}: no cancel");
        // The same operation is reconciled by status, never resent.
        assert_eq!(
            w.status(),
            (State::Completed, Stage::PickupLoaded, "pickup_loaded")
        );
        assert_eq!(w.prompts().len(), 1);
    }
}

#[test]
fn pi_pickup_outside_herdr_is_unavailable_and_arms_nothing() {
    let w = world();
    let r = pi::handoff_pickup(&w.endpoint(), &w.herdr, None, OP.into(), T);
    assert!(
        matches!(r, Err(PickupError::Unavailable(pi::NO_HERDR_PANE))),
        "{r:?}"
    );
    assert!(w.actions().is_empty());
    assert!(w.prompts().is_empty());
}

#[test]
fn pi_pickup_without_herdr_integration_is_unavailable_and_arms_nothing() {
    let w = world();
    w.pane.lock().unwrap().session_file = None;
    let r = w.click(&w.endpoint());
    assert!(
        matches!(r, Err(PickupError::Unavailable(pi::NO_HERDR_SESSION))),
        "{r:?}"
    );
    assert!(w.actions().is_empty());
    assert!(w.prompts().is_empty());
}
