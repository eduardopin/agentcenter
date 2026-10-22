use super::*;
use std::{
    io::Write,
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};

const SID: &str = "11111111-2222-4333-8444-555555555555";
const OP: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER_OP: &str = "ffffffff-bbbb-4ccc-8ddd-eeeeeeeeeeee";
static SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ac-claude-unit-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(d.join("handoffs")).unwrap();
    d
}

fn iso(ms: u64) -> String {
    let (s, ms) = (ms / 1000, ms % 1000);
    let (y, mo, d) = crate::overlay::transcript::civil_from_days((s / 86400) as i64);
    let t = s % 86400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

fn user(ts: u64, sid: &str, text: &str) -> String {
    serde_json::json!({"type":"user","sessionId":sid,"timestamp":iso(ts),
        "message":{"role":"user","content":text}})
    .to_string()
}
fn boundary(ts: u64, sid: &str, trigger: &str) -> String {
    serde_json::json!({"type":"system","subtype":"compact_boundary","sessionId":sid,
        "timestamp":iso(ts),"compactMetadata":{"trigger":trigger,"preTokens":10}})
    .to_string()
}
fn write_call(ts: u64, id: &str, path: &Path) -> String {
    serde_json::json!({"type":"assistant","sessionId":SID,"timestamp":iso(ts),
        "message":{"content":[{"type":"tool_use","id":id,"name":"Write",
        "input":{"file_path":path,"content":"secret body"}}]}})
    .to_string()
}
fn write_result(ts: u64, id: &str, path: &Path, error: bool) -> String {
    let mut b = serde_json::json!({"type":"tool_result","tool_use_id":id,"content":"ok"});
    if error {
        b["is_error"] = true.into();
    }
    serde_json::json!({"type":"user","sessionId":SID,"timestamp":iso(ts),
        "message":{"content":[b]},
        "toolUseResult": if error { Value::from("Error") } else {
            serde_json::json!({"type":"create","filePath":path}) }})
    .to_string()
}

fn transcript(dir: &Path, lines: &[String]) -> PathBuf {
    let p = dir.join("t.jsonl");
    let mut f = File::create(&p).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    p
}

fn compact(lines: &[String], sent: u64) -> Option<Evidence> {
    let d = tmp();
    let t = transcript(&d, lines);
    let r = evidence(Action::Compact, &t, 0, sent, SID, OP, &[], 0).unwrap();
    fs::remove_dir_all(d).unwrap();
    r
}

#[test]
fn compact_needs_manual_boundary_after_send_and_command() {
    let sent = now_ms();
    let cmd = "<command-name>/compact</command-name>\n<command-args></command-args>";
    // Claude writes the command entry after the boundary in file order.
    assert_eq!(
        compact(
            &[boundary(sent + 20, SID, "manual"), user(sent + 5, SID, cmd)],
            sent
        ),
        Some(Evidence::Compacted {
            boundary_ms: sent + 20
        })
    );
    for (lines, why) in [
        (
            vec![user(sent + 5, SID, cmd), boundary(sent + 20, SID, "auto")],
            "auto",
        ),
        (
            vec![boundary(sent - 1, SID, "manual"), user(sent + 5, SID, cmd)],
            "boundary before send",
        ),
        (
            vec![user(sent - 5, SID, cmd), boundary(sent - 1, SID, "manual")],
            "both before send",
        ),
        (vec![boundary(sent + 20, SID, "manual")], "no command entry"),
        (
            vec![
                user(sent + 30, SID, cmd),
                boundary(sent + 20, SID, "manual"),
            ],
            "boundary before command",
        ),
        (
            vec![
                user(sent + 5, OTHER_OP, cmd),
                boundary(sent + 20, OTHER_OP, "manual"),
            ],
            "other session",
        ),
    ] {
        assert_eq!(compact(&lines, sent), None, "{why}");
    }
}

#[test]
fn offset_skips_earlier_bytes_and_shrink_is_reported() {
    let d = tmp();
    let sent = now_ms();
    let cmd = "<command-name>/compact</command-name>";
    let early = [user(sent + 5, SID, cmd), boundary(sent + 20, SID, "manual")];
    let t = transcript(&d, &early);
    let len = fs::metadata(&t).unwrap().len();
    assert_eq!(
        evidence(Action::Compact, &t, len, sent, SID, OP, &[], 0),
        Ok(None)
    );
    assert_eq!(
        evidence(Action::Compact, &t, len + 1, sent, SID, OP, &[], 0),
        Err(code::TRANSCRIPT_REWRITTEN)
    );
    fs::remove_dir_all(d).unwrap();
}

struct Handoff {
    dir: PathBuf,
    sent: u64,
    uid: u32,
}
impl Handoff {
    fn new() -> Self {
        Self {
            dir: tmp(),
            sent: now_ms().saturating_sub(5_000),
            uid: super::super::pi::getuid(),
        }
    }
    fn file(&self, name: &str) -> PathBuf {
        let p = self.dir.join("handoffs").join(name);
        fs::write(&p, b"# handoff\nbody\n").unwrap();
        p
    }
    fn run(&self, op_mark: &str, path: &Path, error: bool) -> Option<Evidence> {
        let s = self.sent;
        let lines = [
            user(
                s + 1,
                SID,
                &format!(
                    "<command-name>/clear-pickup</command-name><command-args>handoff {MARK}{op_mark}</command-args>"
                ),
            ),
            write_call(s + 2, "toolu_1", path),
            write_result(s + 3, "toolu_1", path, error),
        ];
        let t = transcript(&self.dir, &lines);
        let roots = [self.dir.join("handoffs")];
        evidence(Action::Handoff, &t, 0, s, SID, OP, &roots, self.uid).unwrap()
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn handoff_completes_with_hash_and_size_never_content() {
    let h = Handoff::new();
    let p = h.file("h.md");
    let Some(Evidence::Persisted(a)) = h.run(OP, &p, false) else {
        panic!("expected persisted");
    };
    assert_eq!(a.path, p);
    assert_eq!(a.size, 15);
    assert_eq!(a.sha256, hex(&super::super::sha256(b"# handoff\nbody\n")));
    let json = serde_json::to_string(&Evidence::Persisted(a)).unwrap();
    assert!(!json.contains("body"), "{json}");
}

#[test]
fn handoff_negatives_never_complete() {
    let h = Handoff::new();
    let p = h.file("h.md");
    assert_eq!(h.run(OTHER_OP, &p, false), None, "other operation's mark");
    assert_eq!(h.run(OP, &p, true), None, "failed Write");
    let outside = h.dir.join("elsewhere.md");
    fs::write(&outside, b"x").unwrap();
    assert_eq!(h.run(OP, &outside, false), None, "outside allowed roots");
    let dotdot = h.dir.join("handoffs/../elsewhere.md");
    assert_eq!(h.run(OP, &dotdot, false), None, "lexical escape");
    let link = h.dir.join("handoffs/link.md");
    symlink(&p, &link).unwrap();
    assert_eq!(h.run(OP, &link, false), None, "symlink file");
    fs::create_dir(h.dir.join("real")).unwrap();
    fs::write(h.dir.join("real/x.md"), b"x").unwrap();
    symlink(h.dir.join("real"), h.dir.join("handoffs/sub")).unwrap();
    assert_eq!(
        h.run(OP, &h.dir.join("handoffs/sub/x.md"), false),
        None,
        "symlinked directory"
    );
    let other = Handoff {
        dir: tmp(),
        sent: h.sent,
        uid: h.uid + 1,
    };
    let q = other.file("h.md");
    assert_eq!(other.run(OP, &q, false), None, "other UID");
    let late = Handoff {
        dir: tmp(),
        sent: now_ms() + 60_000,
        uid: h.uid,
    };
    let r = late.file("h.md");
    assert_eq!(late.run(OP, &r, false), None, "mtime before send");
}

#[test]
fn write_without_matching_result_path_is_not_evidence() {
    let h = Handoff::new();
    let p = h.file("h.md");
    let q = h.file("other.md");
    let s = h.sent;
    let lines = [
        user(s + 1, SID, &format!("handoff {MARK}{OP}")),
        write_call(s + 2, "toolu_1", &p),
        write_result(s + 3, "toolu_1", &q, false),
        // Write before the mark does not count either.
    ];
    let t = transcript(&h.dir, &lines);
    let roots = [h.dir.join("handoffs")];
    assert_eq!(
        evidence(Action::Handoff, &t, 0, s, SID, OP, &roots, h.uid),
        Ok(None)
    );
    let lines = [
        write_call(s + 1, "toolu_1", &p),
        user(s + 2, SID, &format!("handoff {MARK}{OP}")),
        write_result(s + 3, "toolu_1", &p, false),
    ];
    let t = transcript(&h.dir, &lines);
    assert_eq!(
        evidence(Action::Handoff, &t, 0, s, SID, OP, &roots, h.uid),
        Ok(None)
    );
}

#[test]
fn subject_requires_claude_explicit_pane_and_session() {
    let mut s: crate::session::Session = serde_json::from_value(serde_json::json!({
        "agent":"claude","role":"agent","pid":7,"starttime":9,"cmdline":null,"cwd":"/x",
        "procs":1,"cpu_pct":null,"cpu_share_pct":null,"pss_bytes":null,"swap_pss_bytes":null,
        "read_bps":null,"write_bps":null,"stall_pct":null,"threads_d":null,"io_bound":false,
        "partial":[],"processes":[],
        "overlay":{"agent_session_id":SID,"herdr":{"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1"}}
    }))
    .unwrap();
    let ok = Subject::from_session(&s).unwrap();
    assert_eq!((ok.session_id.as_str(), ok.target.pid), (SID, 7));
    s.overlay.as_mut().unwrap().herdr.as_mut().unwrap().pane_id = Some("current".into());
    assert_eq!(Subject::from_session(&s), Err(code::NO_HERDR_PANE));
    s.overlay.as_mut().unwrap().herdr = None;
    assert_eq!(Subject::from_session(&s), Err(code::NO_HERDR_PANE));
    s.agent = "pi".into();
    assert_eq!(Subject::from_session(&s), Err(code::NOT_CLAUDE));
}

#[test]
fn seal_copy_takes_only_the_acknowledged_bytes() {
    let d = tmp();
    let mut env = Env::new(d.clone());
    env.handoffs = d.join("state/handoffs");
    let original = d.join("handoffs/h.md");
    fs::write(&original, b"acknowledged").unwrap();
    let ack = artifact(&original, &[d.join("handoffs")], 0, env.uid).unwrap();
    // Same size, other bytes: only the sha256 of the ack refuses it, before
    // anything is written.
    fs::write(&original, b"ACKNOWLEDGED").unwrap();
    assert_eq!(seal_copy(&ack, &env, SID, OP), Err(code::COPY_MISMATCH));
    assert!(!env.handoffs.exists(), "nothing written for refused bytes");
    // Changed after the ack: no copy.
    fs::write(&original, b"changed later").unwrap();
    assert_eq!(seal_copy(&ack, &env, SID, OP), Err(code::COPY_MISMATCH));
    // A symlink to identical bytes is not the file that was acknowledged.
    let real = d.join("real.md");
    fs::write(&real, b"acknowledged").unwrap();
    fs::remove_file(&original).unwrap();
    symlink(&real, &original).unwrap();
    assert_eq!(seal_copy(&ack, &env, SID, OP), Err(code::COPY_MISMATCH));
    fs::remove_file(&original).unwrap();
    fs::write(&original, b"acknowledged").unwrap();
    let copy = seal_copy(&ack, &env, SID, OP).unwrap();
    assert_eq!((copy.size, &copy.sha256), (ack.size, &ack.sha256));
    assert_eq!(fs::metadata(&copy.path).unwrap().mode() & 0o777, 0o400);
    assert!(copy_intact(&copy, env.uid));
    // Never over an existing copy.
    assert_eq!(seal_copy(&ack, &env, SID, OP), Err(code::COPY_FAILED));
    // The copy edited in place to other bytes of the same size.
    fs::set_permissions(&copy.path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&copy.path, b"ACKNOWLEDGED").unwrap();
    assert!(!copy_intact(&copy, env.uid));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn pickup_loaded_needs_own_session_prompt_then_successful_read() {
    let d = tmp();
    let copy = d.join("handoffs/s-copy.md");
    let new = "22222222-2222-4333-8444-555555555555";
    let prompt = |sid: &str, ts| {
        user(
            ts,
            sid,
            &format!(
                "<command-name>/clear-pickup</command-name>\n<command-args>pickup {}</command-args>",
                copy.display()
            ),
        )
    };
    let read_of = |ts: u64, sid: &str, error: bool, called: &Path, result: &Path| {
        [
            serde_json::json!({"type":"assistant","sessionId":sid,"timestamp":iso(ts),"message":{"content":[
                {"type":"tool_use","id":"toolu_r","name":"Read","input":{"file_path":called}}]}})
            .to_string(),
            serde_json::json!({"type":"user","sessionId":sid,"timestamp":iso(ts + 1),"message":{"content":[
                {"type":"tool_result","tool_use_id":"toolu_r","is_error":error,"content":"x"}]},
                "toolUseResult":{"type":"text","file":{"filePath":result}}})
            .to_string(),
        ]
    };
    let read = |ts: u64, sid: &str, error: bool| read_of(ts, sid, error, &copy, &copy);
    let check = |lines: Vec<String>| {
        let t = transcript(&d, &lines);
        pickup_loaded(&t, 1_000, new, &copy).unwrap()
    };
    let ok = [vec![prompt(new, 2_000)], read(2_001, new, false).to_vec()].concat();
    assert!(check(ok));
    // Read before the prompt, failed Read, other session, before the send.
    assert!(!check(
        [read(1_500, new, false).to_vec(), vec![prompt(new, 2_000)]].concat()
    ));
    assert!(!check(
        [vec![prompt(new, 2_000)], read(2_001, new, true).to_vec()].concat()
    ));
    assert!(!check(
        [vec![prompt(SID, 2_000)], read(2_001, SID, false).to_vec()].concat()
    ));
    assert!(!check(
        [vec![prompt(new, 500)], read(501, new, false).to_vec()].concat()
    ));
    // Each path is checked on its own: the call's input and the result's file.
    let other = d.join("handoffs/other.md");
    assert!(!check(
        [
            vec![prompt(new, 2_000)],
            read_of(2_001, new, false, &other, &copy).to_vec()
        ]
        .concat()
    ));
    assert!(!check(
        [
            vec![prompt(new, 2_000)],
            read_of(2_001, new, false, &copy, &other).to_vec()
        ]
        .concat()
    ));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn pickup_arg_safe_takes_only_one_plain_argument() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    assert!(pickup_arg_safe(Path::new(
        "/home/u/.local/state/agentcenter/handoffs/s31-aaaa.md"
    )));
    for bad in [
        "",
        "/a b.md",
        "/a<b.md",
        "/a>b.md",
        "/a\tb.md",
        "/a\nb.md",
        "/a\u{a0}b.md",
        "/a\u{1b}b.md",
    ] {
        assert!(!pickup_arg_safe(Path::new(bad)), "{bad:?}");
    }
    assert!(!pickup_arg_safe(Path::new(OsStr::from_bytes(
        b"/a\xffb.md"
    ))));
}

#[test]
fn copies_go_under_xdg_state_home_only_when_absolute() {
    let home = PathBuf::from("/h");
    let at =
        |state: Option<&str>| Env::with_state_home(home.clone(), state.map(Into::into)).handoffs;
    assert_eq!(at(Some("/s")), Path::new("/s/agentcenter/handoffs"));
    for state in [None, Some(""), Some("rel/state")] {
        assert_eq!(
            at(state),
            Path::new("/h/.local/state/agentcenter/handoffs"),
            "{state:?}"
        );
    }
}

#[test]
fn copy_directory_behind_a_symlink_is_a_failed_copy() {
    let d = tmp();
    let original = d.join("handoffs/h.md");
    fs::write(&original, b"acknowledged").unwrap();
    fs::create_dir(d.join("real")).unwrap();
    symlink(d.join("real"), d.join("link")).unwrap();
    let mut env = Env::with_state_home(d.clone(), None);
    env.handoffs = d.join("link/state/handoffs");
    let ack = artifact(&original, &[d.join("handoffs")], 0, env.uid).unwrap();
    assert_eq!(seal_copy(&ack, &env, SID, OP), Err(code::COPY_FAILED));
    assert!(
        !d.join("real/state").exists(),
        "nothing created through the link"
    );
    let _ = fs::remove_dir_all(&d);
}
