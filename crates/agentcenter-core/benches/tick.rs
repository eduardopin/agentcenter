//! Custo de um tick (orcamento: < 5 ms com 500 processos, ARCHITECTURE).

use agentcenter_core::fixture::{TempDir, synthetic};
use agentcenter_core::overlay::claude_state::{ClaudeState, SESSIONS_DIR_ENV};
use agentcenter_core::overlay::herdr::SOCKET_ENV as HERDR_SOCKET_ENV;
use agentcenter_core::overlay::{self, Overlay};
use agentcenter_core::{ProcRoot, Registry, Sampler};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::Path;

/// 40 arquivos de sessao como os do Claude (mais o `.key` de cada): 19 casam
/// com as sessoes `claude` de `synthetic(500)` (pids 3, 23, ..., 363, starttime
/// 1000) e 21 sao de pids mortos.
fn claude_sessions(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let live = (3..=363).step_by(20);
    let dead = 10_000..10_021;
    for (i, pid) in live.chain(dead).enumerate() {
        let kind = if pid < 10_000 { "interactive" } else { "bg" };
        let json = format!(
            "{{\"pid\":{pid},\"sessionId\":\"00000000-0000-4000-8000-{pid:012}\",\
             \"cwd\":\"/home/dev/project-{pid}\",\"startedAt\":1791818122167,\
             \"procStart\":\"1000\",\"version\":\"2.1.300\",\"peerProtocol\":1,\
             \"kind\":\"{kind}\",\"entrypoint\":\"cli\",\"name\":\"project-{i}\",\
             \"nameSource\":\"derived\",\"updatedAt\":1792040464474,\
             \"status\":\"idle\",\"statusUpdatedAt\":1792040464474}}\n"
        );
        std::fs::write(dir.join(format!("{pid}.json")), json).unwrap();
        std::fs::write(dir.join(format!("{pid}.{i:064x}.key")), "k\n").unwrap();
    }
    assert_eq!(
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension() == Some("json".as_ref()))
            .count(),
        40
    );
}

fn tick(c: &mut Criterion) {
    let dir = TempDir::new("ac-bench").unwrap();
    // O overlay do registry olha um diretorio vazio: os ticks abaixo nao
    // dependem de ~/.claude desta maquina.
    let empty = dir.path().join("claude-empty");
    std::fs::create_dir_all(&empty).unwrap();
    // SAFETY: antes de qualquer Sampler e de qualquer thread; o criterion roda
    // as medidas nesta mesma thread.
    unsafe { std::env::set_var(SESSIONS_DIR_ENV, &empty) };
    // O overlay do herdr aponta para um socket que nao existe: nada de
    // conversar com o herdr desta maquina durante a medida.
    let no_herdr = dir.path().join("herdr-ausente.sock");
    // SAFETY: idem acima.
    unsafe { std::env::set_var(HERDR_SOCKET_ENV, &no_herdr) };

    // Duas raizes com contadores diferentes: alternar entre elas gera deltas
    // a cada tick, como no /proc vivo. Com delayacct o stall sai do campo 42;
    // sem, cada thread das sessoes tem o seu task/<tid>/stat lido.
    for (name, delayacct) in [
        ("tick_fixture_500_delayacct", true),
        ("tick_fixture_500_d_state", false),
    ] {
        let a = synthetic(dir.path().join(format!("{name}-a")), 500, 1, delayacct)
            .unwrap()
            .proc_root();
        let b = synthetic(dir.path().join(format!("{name}-b")), 500, 2, delayacct)
            .unwrap()
            .proc_root();
        let mut sampler = Sampler::new(Registry::default());
        let mut flip = false;
        c.bench_function(name, |bench| {
            bench.iter(|| {
                flip = !flip;
                black_box(sampler.tick(if flip { &a } else { &b }).unwrap())
            })
        });
    }

    // Overlay do Claude com 40 arquivos sobre o mesmo /proc de 500 processos:
    // acrescimo = tick_fixture_500_claude_40 - tick_fixture_500_delayacct, e
    // claude_state_40 mede so o overlay (enrich das 19 sessoes + listagem).
    let sessions = dir.path().join("claude-sessions");
    claude_sessions(&sessions);
    let a = synthetic(dir.path().join("claude-a"), 500, 1, true)
        .unwrap()
        .proc_root();
    let b = synthetic(dir.path().join("claude-b"), 500, 2, true)
        .unwrap()
        .proc_root();
    let overlays: Vec<Box<dyn Overlay>> = vec![Box::new(ClaudeState::new(
        Some(sessions.clone()),
        a.clone(),
    ))];
    let state = ClaudeState::new(Some(sessions), a.clone());
    let mut sampler = Sampler::new(Registry::default());
    let mut flip = false;
    c.bench_function("tick_fixture_500_claude_40", |bench| {
        bench.iter(|| {
            flip = !flip;
            let mut snap = sampler.tick(if flip { &a } else { &b }).unwrap();
            overlay::apply(&overlays, &mut snap.sessions);
            black_box(state.background(&snap.sessions));
            black_box(snap)
        })
    });
    let mut snap = sampler.tick(&a).unwrap();
    overlay::apply(&overlays, &mut snap.sessions);
    let named = snap
        .sessions
        .iter()
        .filter(|s| s.overlay.as_ref().is_some_and(|o| o.status.is_some()))
        .count();
    assert_eq!(named, 19, "as 19 sessoes claude casam");
    c.bench_function("claude_state_40", |bench| {
        bench.iter(|| {
            overlay::apply(&overlays, &mut snap.sessions);
            black_box(state.background(&snap.sessions))
        })
    });

    let live = ProcRoot::default();
    let mut sampler = Sampler::new(Registry::default());
    c.bench_function("tick_live_proc", |bench| {
        bench.iter(|| black_box(sampler.tick(&live).unwrap()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = tick
}
criterion_main!(benches);
