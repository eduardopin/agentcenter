//! `Session::started_at_ms`: boot time (`/proc/stat` btime) plus the agent's
//! `starttime` ticks, so the rule engine can age a session without a clock.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::{Registry, Sampler};

fn started_at(btime: Option<u64>, starttime: u64) -> Option<u64> {
    let dir = TempDir::new("ac-started").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(20, 1, "claude");
    p.starttime = starttime;
    fp.add(&p).unwrap();
    if let Some(b) = btime {
        let stat = std::fs::read_to_string(fp.root().join("stat")).unwrap();
        let stat = stat.replace("btime 0", &format!("btime {b}"));
        std::fs::write(fp.root().join("stat"), stat).unwrap();
    }
    let snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    assert_eq!(snap.sessions.len(), 1);
    snap.sessions[0].started_at_ms
}

#[test]
fn started_at_is_boot_time_plus_start_ticks_in_ms() {
    // 360_000 ticks at 100 Hz is 1 h after boot
    assert_eq!(
        started_at(Some(1_000_000), 360_000),
        Some(1_000_000_000 + 3_600_000)
    );
}

#[test]
fn started_at_is_unknown_without_a_boot_time() {
    // the fake /proc/stat says btime 0: unknown, never 1970 + ticks
    assert_eq!(started_at(None, 360_000), None);
}
