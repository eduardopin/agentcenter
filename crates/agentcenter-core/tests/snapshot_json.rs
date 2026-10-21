//! Contrato JSON do snapshot.

use agentcenter_core::overlay::{HerdrOverlay, OverlayStatus, SessionOverlay};
use agentcenter_core::{ProcRoot, Registry, Sampler, Snapshot};
use std::path::Path;

#[test]
fn snapshot_json_has_contract_fields_and_round_trips() {
    let root =
        ProcRoot::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proc/nested"));
    let mut snap = Sampler::new(Registry::default()).tick(&root).unwrap();
    snap.sessions[0].overlay = Some(SessionOverlay {
        status: Some(OverlayStatus::Blocked),
        herdr: Some(HerdrOverlay {
            pane_id: Some("p1".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
    // v2: rss_bytes pode ser null e memoria/threads_d seguem a cadencia de smaps.
    // v3: uid e ctx switches seguem a cadencia do status (C6b).
    // v4: `summary` e campos reservados de tool calls/subagentes no overlay (C13).
    // v5: cockpit por sessao no overlay e agregados no resumo (C21).
    assert_eq!(v["schema_version"], 5);
    assert_eq!(v["machine"]["io_stall_source"], "d_state");
    assert_eq!(v["machine"]["cores"], 4);
    // Fixture sem meminfo, loadavg nem pressure: chaves presentes, valor null.
    for key in [
        "mem_total_bytes",
        "mem_available_bytes",
        "swap_total_bytes",
        "swap_free_bytes",
        "load_avg",
        "psi",
    ] {
        assert!(v["machine"].get(key).is_some_and(|x| x.is_null()), "{key}");
    }
    assert!(v["sessions"][0]["processes"][0]["rss_bytes"].is_u64());
    let s0 = &v["sessions"][0];
    for key in [
        "agent",
        "pid",
        "procs",
        "cpu_pct",
        "pss_bytes",
        "swap_pss_bytes",
        "read_bps",
        "write_bps",
        "stall_pct",
        "threads_d",
        "io_bound",
        "overlay",
        "cwd",
        "processes",
    ] {
        assert!(s0.get(key).is_some(), "falta {key}");
    }
    assert_eq!(s0["overlay"]["status"], "blocked");
    assert_eq!(s0["overlay"]["herdr"]["pane_id"], "p1");
    // Contrato reservado (C14/C15): chave presente, sempre null aqui.
    for key in [
        "tool_calls",
        "tool_calls_open",
        "tool_calls_per_min",
        "subagents_live",
        "subagents_total",
    ] {
        assert!(s0["overlay"].get(key).is_some_and(|x| x.is_null()), "{key}");
    }
    let summary = &v["summary"];
    assert_eq!(summary["sessions"], 2);
    assert_eq!(summary["multiplexers"], 1);
    assert_eq!(summary["by_agent"]["claude"], 2);
    for key in ["tool_calls_open", "tool_calls_per_min", "subagents_live"] {
        assert!(summary.get(key).is_some_and(|x| x.is_null()), "{key}");
    }
    // C21: o provider vem do agente mesmo sem transcript, entao a sessao
    // claude sem nenhum outro overlay tem so ele; o cockpit fica vazio.
    let o1 = &v["sessions"][1]["overlay"];
    assert_eq!(o1["provider"], "anthropic");
    assert_eq!(o1["cockpit_pending"], false);
    for key in ["cockpit", "model", "tokens_in", "context_pct", "status"] {
        assert!(o1.get(key).is_some_and(|x| x.is_null()), "{key}");
    }
    assert_eq!(
        v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["agent"] == "herdr")
            .unwrap()["role"],
        "multiplexer"
    );
    let back: Snapshot = serde_json::from_value(v).unwrap();
    assert_eq!(back, snap);
}

#[test]
fn rss_without_vmrss_is_null_not_zero() {
    use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
    // Zumbi e kernel thread nao tem `mm`: o kernel escreve 0 em vsize (campo 23)
    // e rss (24) no stat e tira o VmRSS do status. Ausente vira null, nao 0.
    let dir = TempDir::new("ac-rss-null").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(20, 1, "claude")).unwrap();
    let mut no_mm = FakeProcess::new(21, 20, "node");
    no_mm.rss_pages = 0;
    fp.add(&no_mm).unwrap();
    let status = fp.root().join("21/status");
    let text = std::fs::read_to_string(&status).unwrap();
    let without: String = text
        .lines()
        .filter(|l| !l.starts_with("VmRSS:"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(without, text);
    std::fs::write(&status, without).unwrap();
    // 22 esta todo em swap: rss 0 com vsize > 0 e uma medida, nao ausencia.
    let mut swapped = FakeProcess::new(22, 20, "node");
    swapped.rss_pages = 0;
    fp.add(&swapped).unwrap();
    let mut fields: Vec<String> = swapped
        .stat_line()
        .split_whitespace()
        .map(String::from)
        .collect();
    fields[22] = (64 << 20).to_string();
    std::fs::write(fp.root().join("22/stat"), fields.join(" ") + "\n").unwrap();

    let snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
    let procs = v["sessions"][0]["processes"].as_array().unwrap();
    let rss = |pid: u32| &procs.iter().find(|p| p["pid"] == pid).unwrap()["rss_bytes"];
    assert!(rss(21).is_null(), "{}", rss(21));
    assert_eq!(rss(22), 0);
    assert_eq!(rss(20), 256 * 4 * 1024);
}

#[test]
fn snapshot_v4_without_cockpit_fields_still_reads() {
    use agentcenter_core::Summary;
    use agentcenter_core::cockpit::{Cockpit, Thresholds, Tokens};
    let root =
        ProcRoot::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proc/nested"));
    let mut snap = Sampler::new(Registry::default()).tick(&root).unwrap();
    let t = Tokens {
        input: 10_000,
        cache_read: 300_000,
        ctx: 120_000,
        window: 200_000,
        ..Default::default()
    };
    snap.sessions[0].overlay = Some(SessionOverlay {
        status: Some(OverlayStatus::Busy),
        provider: Some("anthropic".into()),
        tokens_in: Some(310_000),
        context_pct: Some(60.0),
        cockpit: Some(Cockpit::compute(&Thresholds::default(), &t, None, None)),
        ..Default::default()
    });
    snap.summary = Summary::of(&snap.sessions);
    assert_eq!(snap.summary.cockpit.compactar, 1);

    // O JSON como um leitor v4 o gravaria: sem nenhuma das chaves do C21.
    let mut v = serde_json::to_value(&snap).unwrap();
    v["schema_version"] = 4.into();
    let summary = v["summary"].as_object_mut().unwrap();
    for key in [
        "cockpit",
        "cockpit_pending",
        "sessions_burn_weighted",
        "cache_hit",
        "ctx_pct_max",
    ] {
        assert!(summary.remove(key).is_some(), "{key}");
    }
    for s in v["sessions"].as_array_mut().unwrap() {
        if let Some(o) = s["overlay"].as_object_mut() {
            for key in ["provider", "cockpit", "cockpit_pending"] {
                assert!(o.remove(key).is_some(), "{key}");
            }
        }
    }
    let back: Snapshot = serde_json::from_value(v).unwrap();

    assert_eq!(back.schema_version, 4);
    let mut want = snap.clone();
    want.schema_version = 4;
    want.summary = Summary {
        cockpit: Default::default(),
        cockpit_pending: 0,
        sessions_burn_weighted: None,
        cache_hit: None,
        ctx_pct_max: None,
        ..snap.summary.clone()
    };
    for s in &mut want.sessions {
        if let Some(o) = &mut s.overlay {
            o.provider = None;
            o.cockpit = None;
            o.cockpit_pending = false;
        }
    }
    assert_eq!(back, want);
    // O que nao e do C21 chega inteiro.
    let o = back.sessions[0].overlay.as_ref().unwrap();
    assert_eq!((o.tokens_in, o.context_pct), (Some(310_000), Some(60.0)));
}

/// A fixture da GUI com o cockpit preenchido (gerada por `gen.py`) tem o que o
/// core publicaria: o cockpit inteiro de cada sessao refeito dos seus tokens,
/// com a recomendacao de `recommend`, e a cobertura que o C21 pede.
#[test]
fn gui_cockpit_fixture_matches_the_core_cockpit_of_its_own_numbers() {
    use agentcenter_core::adapters::AgentRole;
    use agentcenter_core::cockpit::{Cockpit, Loops, Recommendation, Thresholds, Tokens};
    use agentcenter_core::cockpit::{ctx_pct, round_n};
    use std::collections::BTreeSet;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agentcenter/tests/fixtures/snapshots/cockpit.json");
    let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let th = Thresholds::default();
    let (mut recs, mut providers) = (Vec::new(), BTreeSet::new());
    let (mut sem_dado, mut pending) = (0, 0);
    for s in snap.sessions.iter().filter(|s| s.role == AgentRole::Agent) {
        let o = s.overlay.as_ref().expect("overlay");
        providers.insert(o.provider.clone());
        let Some(c) = &o.cockpit else {
            assert!(o.tokens_in.is_none(), "pid {}: tokens sem cockpit", s.pid);
            sem_dado += 1;
            pending += usize::from(o.cockpit_pending);
            continue;
        };
        assert!(!o.cockpit_pending, "pid {}: cockpit e pendencia", s.pid);
        let t = Tokens {
            input: o.tokens_in.unwrap() - c.cache_read - c.cache_write,
            cache_write: c.cache_write,
            cache_read: c.cache_read,
            output: o.tokens_out.unwrap(),
            ctx: c.ctx_tokens,
            window: c.window,
            window_inferred: c.window_inferred,
        };
        let handoff = c.handoff_name.as_deref().zip(c.handoff_age_min);
        // O sinal de loop (C23) entra pelos dois numeros, e o motivo LOOP sai
        // deles como no core.
        let loops = Loops {
            max_streak: c.max_streak,
            repeated_max: c.repeated_max,
        };
        assert!(c.max_streak >= 1, "pid {}: transcript lido", s.pid);
        let want = Cockpit::compute_with_loops(&th, &t, loops, c.idle_min, handoff);
        assert_eq!(c, &want, "pid {}", s.pid);
        let pct = ctx_pct(t.ctx, t.window);
        assert_eq!(o.context_pct, Some(round_n(pct, 1)), "pid {}", s.pid);
        recs.push(c.recommendation);
    }
    use Recommendation::*;
    assert_eq!(recs.len(), 5);
    for r in [Continuar, Compactar, Handoff, Ociosa, Loop] {
        assert!(recs.contains(&r), "{r:?}");
    }
    assert_eq!((sem_dado, pending), (2, 1), "uma sem dado e uma pendente");
    for p in [Some("anthropic"), Some("openai"), Some("deepinfra"), None] {
        assert!(providers.contains(&p.map(str::to_owned)), "{p:?}");
    }
    let k = &snap.summary.cockpit;
    let sum = k.continuar + k.compactar + k.handoff + k.ociosa + k.loop_ + k.sem_dado;
    assert_eq!(sum, snap.summary.sessions);
    assert_eq!(snap.summary.cockpit_pending, 1);
}
