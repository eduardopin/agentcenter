//! Motor de achados: paridade com a referencia Python (fixtures) e bordas.

use agentcenter_core::insights::{Finding, facts, fallback, findings};
use agentcenter_core::Snapshot;
use serde_json::{Value, json};
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/insights")
}

fn stems() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().into_string().unwrap();
            n.strip_suffix(".snapshot.json").map(str::to_owned)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn parity_with_python_reference() {
    let stems = stems();
    assert!(stems.len() >= 12, "fixtures missing: {}", stems.len());
    let mut ids = std::collections::BTreeSet::new();
    for stem in stems {
        let snap: Snapshot =
            serde_json::from_str(&std::fs::read_to_string(dir().join(format!("{stem}.snapshot.json"))).unwrap())
                .unwrap_or_else(|e| panic!("{stem}: {e}"));
        let want: Value =
            serde_json::from_str(&std::fs::read_to_string(dir().join(format!("{stem}.expected.json"))).unwrap())
                .unwrap();
        let got = findings(&snap);
        let want_fs = want["findings"].as_array().unwrap();
        assert_eq!(got.len(), want_fs.len(), "{stem}: count");
        for (g, w) in got.iter().zip(want_fs) {
            assert_eq!(g.id, w["id"].as_str().unwrap(), "{stem}: id");
            assert_eq!(g.severity, w["severity"].as_str().unwrap(), "{stem}: severity");
            assert_eq!(g.score as i64, w["score"].as_i64().unwrap(), "{stem}: score");
            assert_eq!(g.text, w["text"].as_str().unwrap(), "{stem}: text");
            let must: Vec<&str> = w["must"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
            assert_eq!(g.must, must, "{stem}: must");
            ids.insert(g.id.clone());
        }
        assert_eq!(facts(&got), want["facts"].as_str().unwrap(), "{stem}: facts");
        assert_eq!(fallback(&got), want["fallback"].as_str().unwrap(), "{stem}: fallback");
    }
    for id in [
        "loop",
        "memory_pressure",
        "context_full",
        "idle_memory",
        "cpu_saturated",
        "burn_leader",
        "cache_cold",
        "all_clear",
    ] {
        assert!(ids.contains(id), "no fixture exercises {id}");
    }
}

const GB: u64 = 1_000_000_000;

/// Maquina folgada: 16 GB totais, 12 GB livres, carga 1.0 em 8 nucleos.
fn machine() -> Value {
    json!({
        "cores": 8, "cpu_pct": null, "io_stall_source": "d_state", "processes": 1,
        "session_processes": 1, "mem_total_bytes": 16 * GB, "mem_available_bytes": 12 * GB,
        "swap_total_bytes": 0, "swap_free_bytes": 0,
        "load_avg": {"one": 1.0, "five": 1.0, "fifteen": 1.0}, "psi": null
    })
}

fn session(pid: u32, cwd: &str, pss: u64, overlay: Value) -> Value {
    json!({
        "agent": "claude", "role": "agent", "pid": pid, "cmdline": null, "cwd": cwd, "procs": 1,
        "cpu_pct": null, "cpu_share_pct": null, "pss_bytes": pss, "swap_pss_bytes": null,
        "read_bps": null, "write_bps": null, "stall_pct": null, "threads_d": null,
        "io_bound": false, "partial": [], "overlay": overlay, "processes": []
    })
}

fn cockpit(extra: Value) -> Value {
    let mut c = json!({
        "burn_weighted": 0, "burn_pct": 0.0, "cache_read": 0, "cache_write": 0, "cache_hit": null,
        "reread_share": 0.0, "reread_ratio": 0.0, "payback_turns": null, "floor_tokens": 0,
        "ctx_tokens": 0, "window": 1000000, "window_inferred": false, "idle_min": null,
        "handoff_age_min": null, "handoff_name": null, "max_streak": 1, "repeated_max": 1,
        "recommendation": "CONTINUAR", "reasons": []
    });
    for (k, v) in extra.as_object().unwrap() {
        c[k] = v.clone();
    }
    c
}

fn overlay(extra: Value, ck: Value) -> Value {
    let mut o = json!({ "status": "busy", "cockpit": cockpit(ck) });
    for (k, v) in extra.as_object().unwrap() {
        o[k] = v.clone();
    }
    o
}

fn snap(machine: Value, sessions: Vec<Value>) -> Snapshot {
    serde_json::from_value(json!({
        "schema_version": 5, "taken_at_ms": 0, "interval_ms": 0,
        "machine": machine, "sessions": sessions
    }))
    .unwrap()
}

fn ids(fs: &[Finding]) -> Vec<&str> {
    fs.iter().map(|f| f.id.as_str()).collect()
}

#[test]
fn empty_session_list_is_all_clear() {
    let fs = findings(&snap(machine(), vec![]));
    assert_eq!(ids(&fs), ["all_clear"]);
    assert_eq!(fs[0].severity, "info");
    assert_eq!(fs[0].score, 0);
    assert_eq!(
        fs[0].text,
        "0 sessions are running with 12.0 GB of memory available and no session needing attention. Action: none."
    );
    assert_eq!(fs[0].must, ["0", "12.0"]);
}

#[test]
fn session_without_overlay_or_cockpit_fires_nothing() {
    let s = vec![
        session(1, "/home/dev/a", GB, Value::Null),
        session(2, "/home/dev/b", GB, json!({ "status": "idle" })),
    ];
    let fs = findings(&snap(machine(), s));
    assert_eq!(ids(&fs), ["all_clear"]);
}

#[test]
fn multiplexers_are_not_counted_as_agents() {
    let mut mux = session(9, "/home/dev/mux", 50 * GB, Value::Null);
    mux["role"] = json!("multiplexer");
    let fs = findings(&snap(machine(), vec![mux]));
    assert_eq!(ids(&fs), ["all_clear"]);
    assert!(fs[0].text.starts_with("0 sessions"));
}

#[test]
fn singular_wording_and_label_fallback() {
    let s = vec![
        session(7, "/home/dev/api", GB, overlay(json!({}), json!({ "max_streak": 9 }))),
        // cwd com barra no fim: basename vazio, cai no nome do agente
        session(8, "/home/dev/", GB, overlay(json!({ "context_pct": 91.4 }), json!({}))),
    ];
    let fs = findings(&snap(machine(), s));
    assert_eq!(ids(&fs), ["loop", "context_full"]);
    assert_eq!(
        fs[0].text,
        "1 session is repeating the same tool call; api (pid 7) repeated it 9 times in a row. Action: interrupt that session and redirect it."
    );
    assert_eq!(fs[0].must, ["9", "7"]);
    assert_eq!(
        fs[1].text,
        "1 session is past 75% of the context window; the fullest is claude (pid 8) at 91%. Action: write a handoff and start a fresh session."
    );
    assert_eq!(fs[1].must, ["91%", "handoff"]);
}

#[test]
fn plural_wording() {
    let s = vec![
        session(1, "/p/a", GB, overlay(json!({}), json!({ "max_streak": 5 }))),
        session(2, "/p/b", GB, overlay(json!({}), json!({ "max_streak": 12 }))),
    ];
    let fs = findings(&snap(machine(), s));
    assert!(fs[0].text.starts_with("2 sessions are repeating"), "{}", fs[0].text);
    assert!(fs[0].text.contains("b (pid 2) repeated it 12 times"));
}

#[test]
fn loop_leader_tie_keeps_the_first_session() {
    let s = vec![
        session(1, "/p/a", GB, overlay(json!({}), json!({ "max_streak": 7 }))),
        session(2, "/p/b", GB, overlay(json!({}), json!({ "max_streak": 7 }))),
    ];
    let fs = findings(&snap(machine(), s));
    assert!(fs[0].text.contains("a (pid 1)"), "{}", fs[0].text);
}

#[test]
fn cap_is_four_ordered_by_score() {
    let mut m = machine();
    m["mem_available_bytes"] = json!(GB);
    m["load_avg"]["one"] = json!(20.0);
    let s: Vec<Value> = (0..3)
        .map(|i| {
            session(
                10 + i,
                &format!("/p/s{i}"),
                3 * GB,
                overlay(
                    json!({ "status": "idle", "context_pct": 80.0, "tokens_in": 2_000_000 }),
                    json!({ "max_streak": 6, "idle_min": 60.0, "burn_weighted": if i == 0 { 1000 } else { 1 }, "cache_hit": 0.5 }),
                ),
            )
        })
        .collect();
    let fs = findings(&snap(m, s));
    // loop 100, memory 90, context 80, idle 60 entram; cpu 55, burn 40 e cache 30 ficam de fora
    assert_eq!(ids(&fs), ["loop", "memory_pressure", "context_full", "idle_memory"]);
    let scores: Vec<u32> = fs.iter().map(|f| f.score).collect();
    assert_eq!(scores, [100, 90, 80, 60]);
}

#[test]
fn ordering_puts_lower_scores_after_higher_ones() {
    let mut m = machine();
    m["load_avg"]["one"] = json!(9.5);
    let s: Vec<Value> = (0..3)
        .map(|i| {
            session(
                20 + i,
                &format!("/p/s{i}"),
                GB,
                overlay(
                    json!({ "tokens_in": 3_000_000 }),
                    json!({ "burn_weighted": if i == 1 { 900 } else { 50 }, "cache_hit": 0.6 }),
                ),
            )
        })
        .collect();
    let fs = findings(&snap(m, s));
    assert_eq!(ids(&fs), ["cpu_saturated", "burn_leader", "cache_cold"]);
    assert_eq!(fs[0].text, "Load average is 9.5 on 8 cores. Action: pause the busiest session with agentcenter-cli signal.");
    assert!(fs[1].text.starts_with("s1 (pid 21) accounts for 90% of the token burn of all 3 sessions."));
    assert!(fs[2].text.contains("3 large sessions have a prompt-cache hit rate under 80%; the lowest is s0 (pid 20) at 60%."));
}

#[test]
fn memory_pressure_text_and_thresholds() {
    let mut m = machine();
    m["mem_available_bytes"] = json!(2_400_000_000u64);
    m["swap_total_bytes"] = json!(4 * GB);
    m["swap_free_bytes"] = json!(GB);
    let fs = findings(&snap(m, vec![session(1, "/p/a", 5 * GB, Value::Null)]));
    assert_eq!(
        fs[0].text,
        "Only 2.4 GB of 16.0 GB memory is available (15%), with 3.0 GB in swap; agent sessions hold 5.0 GB. Action: agentcenter-cli reclaim."
    );
    // 15% exato nao dispara (< 0.15)
    let mut m = machine();
    m["mem_available_bytes"] = json!(2_400_000_000u64);
    m["mem_total_bytes"] = json!(16_000_000_000u64);
    let fs = findings(&snap(m, vec![]));
    assert_eq!(ids(&fs), ["all_clear"]);
}

#[test]
fn cache_hit_zero_counts_as_unknown_like_the_reference() {
    // `(cache_hit or 1) < 0.8` no Python: 0.0 vale 1 e a sessao nao entra em cache_cold
    let s = vec![session(
        1,
        "/p/a",
        GB,
        overlay(json!({ "tokens_in": 5_000_000 }), json!({ "cache_hit": 0.0 })),
    )];
    assert_eq!(ids(&findings(&snap(machine(), s))), ["all_clear"]);
}

#[test]
fn facts_and_fallback_wording() {
    let one = vec![Finding {
        id: "x".into(),
        severity: "high".into(),
        score: 1,
        text: "A.".into(),
        must: vec![],
    }];
    assert_eq!(facts(&one), "1. [high] A.");
    assert_eq!(fallback(&one), "A.");
    let mut two = one.clone();
    two.push(Finding { text: "B.".into(), severity: "low".into(), ..one[0].clone() });
    assert_eq!(facts(&two), "1. [high] A.\n2. [low] B.");
    assert_eq!(fallback(&two), "A. 1 more finding below.");
    two.push(two[1].clone());
    assert_eq!(fallback(&two), "A. 2 more findings below.");
}

#[test]
fn finding_serializes_with_the_contract_fields() {
    let f = &findings(&snap(machine(), vec![]))[0];
    let v = serde_json::to_value(f).unwrap();
    assert_eq!(v["id"], "all_clear");
    assert_eq!(v["severity"], "info");
    assert_eq!(v["score"], 0);
    assert!(v["text"].is_string() && v["must"].is_array());
}
