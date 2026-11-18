//! Pins the briefing output on every insights fixture snapshot (the "scenario" fixture: 12 of
//! them): the chat messages, the exact HTTP request bytes sent to the model, and the briefing
//! for a rules run, an accepted answer and a refused one. The golden file was written from the
//! code before the slm refactor; the refactor must leave it byte-identical.
//! Bless with `BLESS_GOLDEN=1` only when the briefing contract changes on purpose.

use agentcenter_core::Snapshot;
use agentcenter_core::briefing::{Endpoint, brief, messages};
use agentcenter_core::insights::{Finding, fallback, findings};
use serde_json::json;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

mod slm_support;
use slm_support::fake;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/insights")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/briefing_golden.json")
}

/// The shared one-shot server, with the port rewritten so the golden does not depend on it.
fn stub(content: &str) -> (Endpoint, thread::JoinHandle<String>) {
    let (ep, h) = fake(content, Duration::ZERO);
    let port = ep.addr().port();
    let h = thread::spawn(move || {
        h.join()
            .unwrap()
            .replace(&format!("127.0.0.1:{port}"), "127.0.0.1:PORT")
    });
    (ep, h)
}

fn scenario(stem: &str) -> serde_json::Value {
    let snap: Snapshot = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join(format!("{stem}.snapshot.json"))).unwrap(),
    )
    .unwrap();
    let fs: Vec<Finding> = findings(&snap);
    let t = Duration::from_secs(5);
    let rules = brief(&fs, None, t);
    // An answer that is exactly the rule text, and one with an invented number.
    let (ep, h) = stub(&fallback(&fs));
    let echoed = brief(&fs, Some(&ep), t);
    let request = h.join().unwrap();
    let (ep, _h) =
        stub("Nine hundred and 77 sessions are on fire; restart the machine now please.");
    let invented = brief(&fs, Some(&ep), t);
    json!({
        "messages": messages(&fs),
        "request": request,
        "rules": rules,
        "echoed": echoed,
        "invented": invented,
    })
}

#[test]
fn briefing_output_is_byte_identical_on_every_scenario() {
    let mut stems: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().into_string().unwrap();
            n.strip_suffix(".snapshot.json").map(str::to_owned)
        })
        .collect();
    stems.sort();
    assert!(stems.len() >= 12, "fixtures missing: {}", stems.len());
    let mut all = serde_json::Map::new();
    for s in &stems {
        all.insert(s.clone(), scenario(s));
    }
    let got = serde_json::to_string_pretty(&all).unwrap() + "\n";
    if std::env::var("BLESS_GOLDEN").is_ok() {
        std::fs::write(golden_path(), &got).unwrap();
        return;
    }
    let want =
        std::fs::read_to_string(golden_path()).expect("golden missing: run with BLESS_GOLDEN=1");
    assert!(
        got == want,
        "briefing output changed against tests/fixtures/briefing_golden.json"
    );
}
