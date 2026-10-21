use super::*;
use std::{
    io::{BufRead, BufReader},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
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
