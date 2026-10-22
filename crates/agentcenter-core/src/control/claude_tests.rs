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
