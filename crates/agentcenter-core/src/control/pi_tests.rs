use super::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

const OP: &str = "12345678-1234-1234-1234-123456789abc";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    endpoint: Endpoint,
    listener: UnixListener,
}
fn starttime(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    crate::procfs::stat::parse_stat(&stat)
        .unwrap()
        .starttime
        .to_string()
}
/// Existing base whose every component directory() would accept as an ancestor.
fn private_base() -> PathBuf {
    let safe = |base: &PathBuf| {
        base.is_absolute()
            && base.ancestors().all(|p| {
                fs::symlink_metadata(p).is_ok_and(|m| {
                    m.is_dir() && (m.uid() == uid() || m.uid() == 0) && m.mode() & 0o022 == 0
                })
            })
    };
    // The worktree's ancestors may be group-writable and HOME may lack .cache or
    // live under /tmp; do not weaken directory(), pick a base it accepts instead.
    ["HOME", "XDG_RUNTIME_DIR"]
        .iter()
        .filter_map(|key| std::env::var_os(key).map(PathBuf::from))
        .find(safe)
        .expect("HOME or XDG_RUNTIME_DIR must have no group/world-writable ancestor")
}
impl Fixture {
    fn new() -> Self {
        let root = private_base().join(format!(
            ".ac-pi-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let dir = root.join("ac-test");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.join("s");
        let pinned = File::open(&dir).unwrap();
        let listener =
            UnixListener::bind(format!("/proc/self/fd/{}/s", pinned.as_raw_fd())).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let endpoint = serde_json::from_value(json!({
            "protocol_version":2,"session_id":"test","session_file":"/private/session.jsonl",
            "pid":std::process::id(),"process_starttime":starttime(std::process::id()),
            "instance_nonce":"a".repeat(64),"socket":socket,
            "expected":{"managerId":"manager","revision":"1","sessionId":"test","sessionFile":"/private/session.jsonl","leafId":"leaf"},
            "capabilities":{"handoff":{"enabled":true,"disabled_reason":null},"compact":{"enabled":true,"disabled_reason":null}}
        })).unwrap();
        Self {
            root,
            endpoint,
            listener,
        }
    }
    fn reply(&self, value: Value, delay: Duration) -> thread::JoinHandle<Value> {
        let listener = self.listener.try_clone().unwrap();
        thread::spawn(move || {
            // A client that never connects fails the test instead of hanging it.
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("no client connected: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).unwrap();
            let request = serde_json::from_str(&line).unwrap();
            thread::sleep(delay);
            let _ = writeln!(stream, "{value}");
            request
        })
    }
    fn manifest(&self) -> Value {
        json!({"protocol_version":2,"session_id":self.endpoint.session_id,"session_file":self.endpoint.session_file,
            "pid":self.endpoint.pid,"process_starttime":self.endpoint.process_starttime,
            "instance_nonce":"a".repeat(64),"socket":self.endpoint.socket,
            "expected":self.endpoint.expected,"capabilities":self.endpoint.capabilities})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
#[test]
fn control_additive_receipt_metadata_remains_closed() {
    let f = Fixture::new();
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    let mut value = json!({"protocol_version":2,"operation_id":OP,"state":"completed","code":"persisted",
        "completed_at":"2026-10-21T03:00:00.000Z","cost_usd":0.0,"expected":q.expected,
        "artifact_path":format!("/private/handoffs/result-{OP}.md"),
        "metadata":{"expected":q.expected,"capabilities":f.endpoint.capabilities,
            "handoff_directory":"/private/handoffs","model_auth":"configured"}});
    let receipt: Reply = serde_json::from_value(value.clone()).unwrap();
    validate_reply(&receipt, &q).unwrap();
    value["content"] = json!("forbidden");
    assert!(serde_json::from_value::<Reply>(value).is_err());
}

fn accepted() -> Value {
    json!({"protocol_version":2,"operation_id":OP,"state":"accepted","code":"accepted"})
}

#[test]
fn control_real_socket_peercred_and_status_cancel_identity() {
    let f = Fixture::new();
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    for request in [q.clone(), q.status(), q.cancel()] {
        let server = f.reply(accepted(), Duration::ZERO);
        let reply = exchange(&f.endpoint, &request, Duration::from_secs(1)).unwrap();
        assert_eq!(reply.state, State::Accepted);
        let wire = server.join().unwrap();
        assert_eq!(wire["operation_id"], OP);
        assert_eq!(wire["expected"], serde_json::to_value(&q.expected).unwrap());
        assert_eq!(
            wire["action"],
            serde_json::to_value(request.action).unwrap()
        );
    }
}

#[test]
fn control_so_peercred_rejects_other_live_pid_before_sending() {
    let mut f = Fixture::new();
    let mut child = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .unwrap();
    f.endpoint.pid = child.id();
    f.endpoint.process_starttime = starttime(child.id());
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    let result = exchange(&f.endpoint, &q, Duration::from_secs(1));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(result, Err(Error::Identity)), "{result:?}");
    // Bounded accept: a rejection before connect must fail here, not hang the suite.
    f.listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut stream = loop {
        match f.listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("exchange never connected before rejecting: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn control_saturated_backlog_returns_before_send_without_blocking() {
    unsafe extern "C" {
        fn listen(fd: i32, backlog: i32) -> i32;
    }
    let f = Fixture::new();
    // A zero backlog admits one queued client on Linux; never accept it.
    assert_eq!(unsafe { listen(f.listener.as_raw_fd(), 0) }, 0);
    let pinned = File::open(f.endpoint.socket.parent().unwrap()).unwrap();
    let _queued = UnixStream::connect(format!("/proc/self/fd/{}/s", pinned.as_raw_fd())).unwrap();
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    let started = Instant::now();
    // Run the production exchange on this thread: no abandoned or killed worker.
    let result = exchange(&f.endpoint, &q, Duration::from_millis(30));
    assert!(
        matches!(result, Err(Error::Io(io::ErrorKind::WouldBlock))),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(350));
}

#[test]
fn control_timeout_after_actual_request_is_unknown() {
    let f = Fixture::new();
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    let server = f.reply(accepted(), Duration::from_millis(120));
    assert!(matches!(
        exchange(&f.endpoint, &q, Duration::from_millis(30)),
        Err(Error::Unknown)
    ));
    assert_eq!(server.join().unwrap()["operation_id"], OP);
}

#[test]
fn control_wrong_operation_and_invalid_completions_are_unknown_over_socket() {
    let f = Fixture::new();
    let q = f.endpoint.request(OP.into(), Action::Handoff).unwrap();
    let metadata = json!({"expected":q.expected,"capabilities":f.endpoint.capabilities});
    for value in [
        json!({"protocol_version":2,"operation_id":"aaaaaaaa-1234-1234-1234-123456789abc","state":"accepted","code":"accepted"}),
        json!({"protocol_version":2,"operation_id":OP,"state":"completed","code":"persisted","metadata":metadata,"expected":q.expected,"artifact_path":"relative.md"}),
        json!({"protocol_version":2,"operation_id":OP,"state":"completed","code":"compacted","metadata":metadata,"compaction":{"capability":"agentcenter.compactGuarded.v1","sessionId":"other","sessionFile":q.session_file,"previousLeafId":"leaf","entryId":"entry"}}),
        json!({"protocol_version":2,"operation_id":OP,"state":"completed","code":"persisted"}),
    ] {
        let server = f.reply(value, Duration::ZERO);
        assert!(matches!(
            exchange(&f.endpoint, &q, Duration::from_secs(1)),
            Err(Error::Unknown)
        ));
        assert_eq!(server.join().unwrap()["operation_id"], OP);
    }
}

#[test]
fn control_discovery_ignores_hostile_manifests_and_symlinks() {
    let f = Fixture::new();
    let path = f.root.join("ac-test/manifest.json");
    fs::write(&path, f.manifest().to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(discover(&f.root).unwrap().len(), 1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(discover(&f.root).unwrap().is_empty());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut hostile = f.manifest();
    hostile["socket"] = json!("/tmp/s");
    fs::write(&path, hostile.to_string()).unwrap();
    assert!(discover(&f.root).unwrap().is_empty());
    fs::remove_file(&path).unwrap();
    let outside = f.root.join("outside.json");
    fs::write(&outside, f.manifest().to_string()).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(outside, &path).unwrap();
    assert!(discover(&f.root).unwrap().is_empty());
    std::os::unix::fs::symlink(f.root.join("ac-test"), f.root.join("ac-link")).unwrap();
    assert!(discover(&f.root).unwrap().is_empty());
}

// ---- Protocol 3 pickup (CC5) ----

fn pickup_request(f: &Fixture, action: Action) -> Request {
    let mut q = f.endpoint.request(OP.into(), action).unwrap();
    q.protocol_version = 3;
    q
}
/// A completed pickup reply as the extension sends it, with a real handoff file.
fn loaded(f: &Fixture) -> (Value, PathBuf) {
    let handoff = f.root.join(format!("s74-0011-{OP}.md"));
    fs::write(&handoff, b"synthetic handoff body").unwrap();
    let value = json!({"protocol_version":3,"operation_id":OP,"state":"completed","code":"pickup_loaded",
        "pickup":{"stage":"pickup_loaded",
            "old_session":{"session_id":"test","session_file":"/private/session.jsonl"},
            "handoff":{"path":handoff,"sha256":"ab".repeat(32),"size":22},
            "new_session":{"session_id":"next","session_file":"/private/next.jsonl","parent_session":"/private/session.jsonl"},
            "loaded_entry_id":"entry1"},
        "metadata":{"expected":f.endpoint.expected,"capabilities":f.endpoint.capabilities,
            "handoff_directory":"/private/handoffs","model_auth":"configured"}});
    (value, handoff)
}

#[test]
fn control_pickup_v3_reply_maps_to_progress_with_evidence() {
    let f = Fixture::new();
    let q = pickup_request(&f, Action::Status);
    let (value, handoff) = loaded(&f);
    let reply: Reply = serde_json::from_value(value).unwrap();
    validate_reply(&reply, &q).unwrap();
    let (p, e) = progress(&reply).unwrap();
    assert_eq!(
        (p.state, p.stage, p.code),
        (
            State::Completed,
            pickup::Stage::PickupLoaded,
            "pickup_loaded"
        )
    );
    assert_eq!(
        p.old_session.session_file,
        PathBuf::from("/private/session.jsonl")
    );
    assert_eq!(p.new_session_id.as_deref(), Some("next"));
    let artifact = p.handoff.unwrap();
    assert_eq!((artifact.path, artifact.size), (handoff, 22));
    assert_eq!(
        e.parent_session,
        Some(PathBuf::from("/private/session.jsonl"))
    );
    assert_eq!(
        e.new_session_file,
        Some(PathBuf::from("/private/next.jsonl"))
    );
    assert_eq!(e.loaded_entry_id.as_deref(), Some("entry1"));
}

#[test]
fn control_pickup_v3_incoherent_records_are_rejected() {
    let f = Fixture::new();
    let q = pickup_request(&f, Action::Status);
    let (base, _) = loaded(&f);
    let check = |edit: &dyn Fn(&mut Value), q: &Request| {
        let mut v = base.clone();
        edit(&mut v);
        serde_json::from_value::<Reply>(v)
            .map_err(|_| Error::Invalid)
            .and_then(|r| validate_reply(&r, q))
            .is_err()
    };
    type Edit = dyn Fn(&mut Value);
    let cases: &[(&str, &Edit)] = &[
        ("parent", &|v| {
            v["pickup"]["new_session"]["parent_session"] = json!("/other.jsonl")
        }),
        ("no entry", &|v| {
            v["pickup"]["loaded_entry_id"] = json!(null)
        }),
        ("same session", &|v| {
            v["pickup"]["new_session"]["session_id"] = json!("test")
        }),
        ("handoff early", &|v| {
            v["pickup"]["stage"] = json!("handoff_running");
            v["state"] = json!("running");
            v["code"] = json!("generating");
        }),
        ("other op file", &|v| {
            v["pickup"]["handoff"]["path"] =
                json!("/private/x-00000000-0000-0000-0000-000000000000.md")
        }),
        ("bad sha", &|v| {
            v["pickup"]["handoff"]["sha256"] = json!("XY")
        }),
        ("unknown stage", &|v| {
            v["pickup"]["stage"] = json!("resumed")
        }),
        ("completed wrong code", &|v| v["code"] = json!("persisted")),
        ("v2 echo", &|v| v["protocol_version"] = json!(2)),
        ("content", &|v| {
            v["pickup"]["handoff"]["text"] = json!("forbidden")
        }),
    ];
    for (name, edit) in cases {
        assert!(check(edit, &q), "{name} must be rejected");
    }
    // A v2 request never accepts a pickup record nor pickup codes.
    let v2 = f.endpoint.request(OP.into(), Action::Status).unwrap();
    assert!(check(&|v| v["protocol_version"] = json!(2), &v2));
    let armed = json!({"protocol_version":2,"operation_id":OP,"state":"accepted","code":"armed"});
    assert!(validate_reply(&serde_json::from_value(armed).unwrap(), &v2).is_err());
}

#[test]
fn control_pickup_protocol_and_herdr_gates_refuse_before_sending() {
    let f = Fixture::new();
    // v2 cannot carry handoff_pickup; v3 carries only pickup/status/cancel.
    let v2 = f
        .endpoint
        .request(OP.into(), Action::HandoffPickup)
        .unwrap();
    assert!(matches!(
        exchange(&f.endpoint, &v2, Duration::from_secs(1)),
        Err(Error::Invalid)
    ));
    let v3 = pickup_request(&f, Action::Handoff);
    assert!(matches!(
        exchange(&f.endpoint, &v3, Duration::from_secs(1)),
        Err(Error::Invalid)
    ));
    // Pi outside Herdr: pickup is not offered and nothing is armed.
    assert_eq!(pickup_unavailable(&f.endpoint, None), Some(NO_HERDR_PANE));
    let herdr = herdr::Endpoint {
        socket: f.root.join("absent"),
        server_pid: 1,
        server_starttime: 1,
    };
    let r = handoff_pickup(&f.endpoint, &herdr, None, OP.into(), Duration::from_secs(1));
    assert!(matches!(r, Err(PickupError::Unavailable(NO_HERDR_PANE))));
    // A pane whose process is another instance (starttime differs) is refused too.
    let other = herdr::Target {
        pid: f.endpoint.pid,
        starttime: 1,
        adapter: "pi".into(),
        workspace_id: "w".into(),
        tab_id: "t".into(),
        pane_id: "p".into(),
    };
    assert_eq!(
        pickup_unavailable(&f.endpoint, Some(&other)),
        Some("identity")
    );
    let r = handoff_pickup(
        &f.endpoint,
        &herdr,
        Some(&other),
        OP.into(),
        Duration::from_secs(1),
    );
    assert!(matches!(r, Err(PickupError::Unavailable("identity"))));
}

/// Replies the real extension sends to refused arms, captured by
/// extensions/pi-control/pickup-unit.test.mjs against the same identity.
const ARM_REFUSALS_FIXTURE: &str = include_str!("../../tests/fixtures/pi/arm_refusals.json");
fn same_process(f: &Fixture) -> herdr::Target {
    herdr::Target {
        pid: f.endpoint.pid,
        starttime: f.endpoint.process_starttime.parse().unwrap(),
        adapter: "pi".into(),
        workspace_id: "w".into(),
        tab_id: "t".into(),
        pane_id: "p".into(),
    }
}
/// Unavailable pickups never reach Herdr: an absent socket proves it.
fn absent_herdr(f: &Fixture) -> herdr::Endpoint {
    herdr::Endpoint {
        socket: f.root.join("absent"),
        server_pid: 1,
        server_starttime: 1,
    }
}
/// Herdr answering the pre-arm read for [`same_process`] with the endpoint's
/// session file (`kind:"path"`, as Herdr 0.9 reports Pi) and counting prompts.
struct FakeHerdr {
    endpoint: herdr::Endpoint,
    prompts: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}
impl FakeHerdr {
    fn new(f: &Fixture) -> Self {
        let socket = f.root.join("herdr");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (pid, file) = (f.endpoint.pid, f.endpoint.session_file.clone());
        let prompts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, done) = (prompts.clone(), stop.clone());
        let server = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut s, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                let mut line = String::new();
                if BufReader::new(&mut s).read_line(&mut line).is_err() || line.is_empty() {
                    continue;
                }
                let q: Value = serde_json::from_str(&line).unwrap();
                let agent = json!({"agent":"pi","pane_id":"p","workspace_id":"w","tab_id":"t",
                    "agent_status":"idle","agent_session":{"kind":"path","value":file}});
                let result = match q["method"].as_str().unwrap() {
                    "agent.list" => json!({"type":"agent_list","agents":[agent]}),
                    "pane.process_info" => json!({"type":"pane_process_info","process_info":
                        {"pane_id":"p","foreground_processes":[{"pid":pid}]}}),
                    "agent.get" => json!({"type":"agent_info","agent":agent}),
                    "agent.prompt" => {
                        count.fetch_add(1, Ordering::Relaxed);
                        json!({"type":"agent_prompted","agent":agent})
                    }
                    m => panic!("unexpected method {m}"),
                };
                let _ = writeln!(s, "{}", json!({"id":q["id"],"result":result}));
            }
        });
        Self {
            endpoint: herdr::Endpoint {
                socket,
                server_pid: pid,
                server_starttime: f.endpoint.process_starttime.parse().unwrap(),
            },
            prompts,
            stop,
            server: Some(server),
        }
    }
}
impl Drop for FakeHerdr {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.server.take() {
            h.join().unwrap();
        }
    }
}

#[test]
fn control_pickup_arm_refusals_from_the_extension_are_refused_not_unknown() {
    let f = Fixture::new();
    let herdr = FakeHerdr::new(&f);
    let fixture: serde_json::Map<String, Value> =
        serde_json::from_str(ARM_REFUSALS_FIXTURE).unwrap();
    for code in [
        "busy",
        "stale_revision",
        "disabled",
        "unsupported_guarded_compaction",
        "capacity",
        "duplicate_mismatch",
        "expired",
    ] {
        let value = fixture[code].clone();
        assert!(
            value["metadata"].is_object(),
            "{code}: authenticated refusal"
        );
        let server = f.reply(value, Duration::ZERO);
        let r = handoff_pickup(
            &f.endpoint,
            &herdr.endpoint,
            Some(&same_process(&f)),
            OP.into(),
            Duration::from_secs(1),
        );
        let wire = server.join().unwrap();
        assert_eq!(
            (wire["action"].as_str(), wire["protocol_version"].as_u64()),
            (Some("handoff_pickup"), Some(3))
        );
        let Err(PickupError::Refused(reply)) = r else {
            panic!("{code}: {r:?}");
        };
        assert_eq!(reply.code, code);
        let state = if code == "expired" {
            State::Unknown
        } else {
            State::Rejected
        };
        assert_eq!(reply.state, state, "{code}");
        assert!(reply.pickup.is_none());
    }
    // Herdr was only read: a refused arm never types anything.
    assert_eq!(herdr.prompts.load(Ordering::Relaxed), 0);
}

#[test]
fn control_pickup_arm_without_record_must_be_a_refusal() {
    let f = Fixture::new();
    let q = pickup_request(&f, Action::HandoffPickup);
    let fixture: serde_json::Map<String, Value> =
        serde_json::from_str(ARM_REFUSALS_FIXTURE).unwrap();
    let check = |state: &str, code: &str| {
        let mut v = fixture["busy"].clone();
        v["state"] = json!(state);
        v["code"] = json!(code);
        validate_reply(&serde_json::from_value(v).unwrap(), &q).is_ok()
    };
    assert!(check("rejected", "busy"));
    // Listener refusals before dispatch carry no metadata.
    let bare =
        json!({"protocol_version":3,"operation_id":OP,"state":"rejected","code":"unauthorized"});
    validate_reply(&serde_json::from_value(bare).unwrap(), &q).unwrap();
    // Anything but a refusal must carry the record.
    for (state, code) in [
        ("accepted", "armed"),
        ("accepted", "accepted"),
        ("rejected", "arm_expired"),
        ("failed", "failed"),
        ("unknown", "unknown_operation"),
        ("rejected", "persisted"),
    ] {
        assert!(!check(state, code), "{state}/{code}");
    }
}

#[test]
fn control_pickup_unavailable_follows_the_manifest_handoff_capability() {
    let mut f = Fixture::new();
    let target = same_process(&f);
    assert_eq!(pickup_unavailable(&f.endpoint, Some(&target)), None);
    for (reason, expected) in [
        (Some("busy"), "busy"),
        (Some("no_model"), "no_model"),
        (Some("model_auth_required"), "model_auth_required"),
        (
            Some("unsupported_guarded_compaction"),
            "unsupported_guarded_compaction",
        ),
        (Some("something_new"), "disabled"),
        (None, "disabled"),
    ] {
        f.endpoint.capabilities.handoff = Capability {
            cancel_supported: false,
            enabled: false,
            disabled_reason: reason.map(str::to_owned),
        };
        assert_eq!(
            pickup_unavailable(&f.endpoint, Some(&target)),
            Some(expected)
        );
        // Not offered: nothing is armed, nothing is sent.
        let r = handoff_pickup(
            &f.endpoint,
            &absent_herdr(&f),
            Some(&target),
            OP.into(),
            Duration::from_secs(1),
        );
        assert!(
            matches!(r, Err(PickupError::Unavailable(e)) if e == expected),
            "{r:?}"
        );
    }
}

#[test]
fn control_pickup_against_a_protocol_2_extension_is_unsupported_not_unknown() {
    let f = Fixture::new();
    let herdr = FakeHerdr::new(&f);
    // What the CC1 listener answers a v3 request: version 2, before dispatch.
    for code in ["invalid_request", "incompatible"] {
        let server = f.reply(
            json!({"protocol_version":2,"operation_id":OP,"state":"rejected","code":code}),
            Duration::ZERO,
        );
        let r = handoff_pickup(
            &f.endpoint,
            &herdr.endpoint,
            Some(&same_process(&f)),
            OP.into(),
            Duration::from_secs(1),
        );
        server.join().unwrap();
        assert!(
            matches!(r, Err(PickupError::Unavailable(UNSUPPORTED_PROTOCOL))),
            "{code}: {r:?}"
        );
        let server = f.reply(
            json!({"protocol_version":2,"operation_id":OP,"state":"rejected","code":code}),
            Duration::ZERO,
        );
        let status = exchange(
            &f.endpoint,
            &pickup_request(&f, Action::Status),
            Duration::from_secs(1),
        );
        server.join().unwrap();
        assert!(
            matches!(status, Err(Error::UnsupportedProtocol)),
            "{status:?}"
        );
    }
    // Anything else in version 2 stays ambiguous: it may come from elsewhere.
    for value in [
        json!({"protocol_version":2,"operation_id":OP,"state":"rejected","code":"busy"}),
        json!({"protocol_version":2,"operation_id":OP,"state":"accepted","code":"accepted"}),
        json!({"protocol_version":2,"operation_id":OP,"state":"rejected","code":"incompatible",
            "metadata":{"expected":f.endpoint.expected,"capabilities":f.endpoint.capabilities}}),
    ] {
        let server = f.reply(value, Duration::ZERO);
        let r = exchange(
            &f.endpoint,
            &pickup_request(&f, Action::HandoffPickup),
            Duration::from_secs(1),
        );
        server.join().unwrap();
        assert!(matches!(r, Err(Error::Unknown)), "{r:?}");
    }
    assert_eq!(herdr.prompts.load(Ordering::Relaxed), 0);
}
