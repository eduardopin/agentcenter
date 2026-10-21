use super::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::atomic::{AtomicU64, Ordering},
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
impl Fixture {
    fn new() -> Self {
        // The worktree's ancestors may be group-writable; do not weaken directory().
        let root = PathBuf::from(std::env::var_os("HOME").unwrap())
            .join(".cache")
            .join(format!(
                "control-test-{}-{}",
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
            let (mut stream, _) = listener.accept().unwrap();
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
    let (mut stream, _) = f.listener.accept().unwrap();
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
