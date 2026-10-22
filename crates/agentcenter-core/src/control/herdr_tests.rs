use super::*;
use std::{
    io::{BufRead, BufReader},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
const PI_FILE: &str = "/home/u/.pi/agent/sessions/--w--/2026-10-22_pi.jsonl";
fn fixture(mode: &'static str) -> (Endpoint, Target, std::thread::JoinHandle<usize>, PathBuf) {
    let root = PathBuf::from(std::env::var_os("HOME").unwrap()).join(format!(
        ".ac-herdr-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(root.join("s")).unwrap();
    fs::set_permissions(root.join("s"), fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let pid = std::process::id();
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = crate::procfs::stat::parse_stat(&text).unwrap().starttime;
    let endpoint = Endpoint {
        socket: root.join("s"),
        server_pid: pid,
        server_starttime: start,
    };
    let target = Target {
        pid,
        starttime: start,
        adapter: "pi".into(),
        workspace_id: "w1".into(),
        tab_id: "w1:t1".into(),
        pane_id: "w1:p1".into(),
    };
    let handle = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_millis(400);
        let mut count = 0;
        let mut lists = 0;
        while Instant::now() < end {
            let Ok((mut s, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            s.set_read_timeout(Some(Duration::from_millis(100)))
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
            let result = match q["method"].as_str().unwrap() {
                "agent.list" => {
                    lists += 1;
                    json!({"agents":[{"agent":"pi","pane_id":"w1:p1","workspace_id":"w1","tab_id": if mode == "moved" && lists == 2 { "w1:t2" } else { "w1:t1" }}]})
                }
                "pane.process_info" => {
                    json!({"process_info":{"pane_id":"w1:p1","foreground_processes":[{"pid":pid}]}})
                }
                "pane.focus" => {
                    assert_eq!(q["params"], json!({"pane_id":"w1:p1"}));
                    count += 1;
                    json!({"type":"pane_focused","pane_id":if mode == "bad_reply" { "w1:p2" } else { "w1:p1" },"workspace_id":"w1"})
                }
                "agent.get" => {
                    assert_eq!(q["params"], json!({"target":"w1:p1"}));
                    let session = match mode {
                        "path" => {
                            json!({"source":"hook","agent":"pi","kind":"path","value":PI_FILE})
                        }
                        "id_as_path" => {
                            json!({"source":"hook","agent":"pi","kind":"id","value":PI_FILE})
                        }
                        "other_kind" => {
                            json!({"source":"hook","agent":"pi","kind":"file","value":PI_FILE})
                        }
                        _ => Value::Null,
                    };
                    json!({"type":"agent_info","agent":{"agent":"pi","pane_id":"w1:p1","workspace_id":"w1",
                        "tab_id":"w1:t1","agent_status":"idle","agent_session":session}})
                }
                "agent.prompt" => {
                    count += 1;
                    json!({"type":"agent_prompted","agent":{"pane_id":"w1:p1"}})
                }
                _ => panic!("unexpected method"),
            };
            let mut response = serde_json::to_vec(&json!({"id":q["id"],"result":result})).unwrap();
            response.push(b'\n');
            s.write_all(&response).unwrap();
        }
        count
    });
    (endpoint, target, handle, root)
}
#[test]
fn herdr_focus_revalidates_and_checks_semantic_reply() {
    for (mode, expected, effects) in [
        ("ok", Ok(()), 1),
        ("moved", Err(Error::Stale), 0),
        ("bad_reply", Err(Error::Unknown), 1),
    ] {
        let (e, t, h, root) = fixture(mode);
        assert_eq!(e.focus(&t, Duration::from_millis(250)), expected);
        assert_eq!(h.join().unwrap(), effects);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn herdr_reused_pid_is_refused_without_effect() {
    let (e, mut t, h, root) = fixture("ok");
    t.starttime += 1;
    assert_eq!(
        e.focus(&t, Duration::from_millis(250)),
        Err(Error::Identity)
    );
    assert_eq!(h.join().unwrap(), 0);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn herdr_pi_pane_is_identified_by_its_session_file() {
    // Herdr 0.9 reports a Pi pane as {kind:"path", value:<session file>}.
    let file = Path::new(PI_FILE);
    for (mode, session, expected, effects) in [
        ("path", AgentSession::Path(file), Ok(()), 1),
        // The same text under another kind is not the same identity.
        ("path", AgentSession::Id(PI_FILE), Err(Error::Session), 0),
        (
            "id_as_path",
            AgentSession::Path(file),
            Err(Error::Session),
            0,
        ),
        (
            "other_kind",
            AgentSession::Path(file),
            Err(Error::Session),
            0,
        ),
        // Pi without Herdr's integration reports no session at all.
        ("null", AgentSession::Path(file), Err(Error::Session), 0),
        (
            "path",
            AgentSession::Path(Path::new("/home/u/other.jsonl")),
            Err(Error::Session),
            0,
        ),
        (
            "path",
            AgentSession::Path(Path::new("rel.jsonl")),
            Err(Error::Invalid),
            0,
        ),
    ] {
        let (e, t, h, root) = fixture(mode);
        let r = e.prompt(
            &t,
            session,
            "/agentcenter-pickup x",
            Duration::from_millis(250),
        );
        assert_eq!(r, expected, "{mode} {session:?}");
        assert_eq!(h.join().unwrap(), effects, "{mode} {session:?}");
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn herdr_pane_reported_keeps_the_kind_and_pane_session_only_ids() {
    for (mode, reported) in [
        ("path", Some(ReportedSession::Path(PI_FILE.into()))),
        ("id_as_path", Some(ReportedSession::Id(PI_FILE.into()))),
        ("other_kind", None),
        ("null", None),
    ] {
        let (e, t, h, root) = fixture(mode);
        let r = e.pane_reported(&t, Duration::from_millis(250)).unwrap();
        assert_eq!(r, (reported.clone(), Some("idle".into())), "{mode}");
        let id = e.pane_session(&t, Duration::from_millis(250)).unwrap().0;
        let expected = match reported {
            Some(ReportedSession::Id(id)) => Some(id),
            _ => None,
        };
        assert_eq!(id, expected, "{mode}");
        assert_eq!(h.join().unwrap(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
