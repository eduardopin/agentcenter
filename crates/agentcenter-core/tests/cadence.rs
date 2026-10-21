//! Sampler continuo: cadencia de smaps_rollup e task/, maquina (meminfo, PSI),
//! historico e a thread com canal.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::{HISTORY_LEN, Registry, Sampler, SamplerConfig};
use std::time::{Duration, Instant};

/// Duas sessoes claude (20 com um node de 4 threads, 30 com 4 threads),
/// delayacct desligado para a varredura de task/ entrar na conta.
fn two_sessions(dir: &TempDir) -> (FakeProc, Vec<FakeProcess>) {
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut node = FakeProcess::new(21, 20, "node");
    node.threads = 4;
    let mut other = FakeProcess::new(30, 1, "claude");
    other.threads = 4;
    let procs = vec![FakeProcess::new(20, 1, "claude"), node, other];
    for p in &procs {
        fp.add(p).unwrap();
    }
    (fp, procs)
}

/// Reescreve os processos com RSS e swap diferentes: memoria que mexe.
fn churn(fp: &FakeProc, procs: &mut [FakeProcess], i: u64) {
    for p in procs.iter_mut() {
        p.rss_pages = 10_000 + 1_000 * i;
        p.swap_kb = 512 * i;
        fp.add(p).unwrap();
    }
}

/// Disco andando (`read_bytes`) em todos os processos, sem mexer na CPU: as
/// sessoes nao ficam quietas e sao lidas todo tick (ver o topo de sampler.rs).
fn busy(fp: &FakeProc, procs: &mut [FakeProcess], t: u64) {
    for p in procs.iter_mut() {
        p.io = Some([4096 * (t + 1), 0, 0, 0]);
        fp.add(p).unwrap();
    }
}

#[test]
fn smaps_every_tick_for_selected_and_every_5_ticks_for_the_rest() {
    let dir = TempDir::new("ac-cadence").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    s.select(Some(20));
    for i in 0..10 {
        churn(&fp, &mut procs, i);
        let snap = s.tick(&root).unwrap();
        assert_eq!(snap.sessions.len(), 2);
        // Entre releituras o valor anterior continua publicado, nao some.
        assert!(snap.sessions.iter().all(|x| x.pss_bytes.is_some()));
    }
    assert_eq!(root.read_count(20, "smaps_rollup"), 10, "selecionada");
    assert_eq!(
        root.read_count(21, "smaps_rollup"),
        10,
        "filho da selecionada"
    );
    assert_eq!(root.read_count(30, "smaps_rollup"), 2, "nao selecionada");
    // Varredura de threads em D: mesma cadencia (20 tem 1 thread, sem task/).
    assert_eq!(root.read_count(21, "task"), 10);
    assert_eq!(root.read_count(30, "task"), 2);
    assert_eq!(root.read_count(20, "task"), 0);

    // Trocar a selecao vale no tick seguinte.
    s.select(Some(30));
    for i in 10..13 {
        churn(&fp, &mut procs, i);
        s.tick(&root).unwrap();
    }
    assert_eq!(root.read_count(30, "smaps_rollup"), 5);
    assert_eq!(
        root.read_count(20, "smaps_rollup"),
        10,
        "ainda dentro dos 5 ticks"
    );
}

#[test]
fn idle_memory_rereads_smaps_only_at_max_age_spread_by_pid() {
    let dir = TempDir::new("ac-cadence-idle").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let mut reads_at: Vec<u64> = Vec::new();
    let mut last = 0;
    for t in 0..700 {
        let snap = s.tick(&root).unwrap();
        let other = snap.sessions.iter().find(|x| x.pid == 30).unwrap();
        assert_eq!(
            other.pss_bytes,
            Some(1024 * 1024),
            "valor em cache, tick {t}"
        );
        let n = root.read_count(30, "smaps_rollup");
        if n != last {
            reads_at.push(t);
            last = n;
        }
    }
    // Primeira leitura na hora; depois so pelo limite de idade (300 ticks a 1 s).
    assert_eq!(reads_at[0], 0);
    assert!(reads_at.len() >= 3, "{reads_at:?}");
    for w in reads_at.windows(2) {
        let gap = w[1] - w[0];
        assert!((5..=300).contains(&gap), "intervalo {gap} em {reads_at:?}");
    }
    // pids diferentes nao releem todos no mesmo tick. Sessoes ativas (disco
    // andando): lidas todo tick, cada pid rele na sua vez. A sessao quieta, lida
    // so na varredura completa, tem o teste dela abaixo.
    let mut first_forced = Vec::new();
    for pid in [20, 21, 30] {
        let r = fp.counting_root();
        let mut s = Sampler::new(Registry::default());
        for t in 0..300u64 {
            busy(&fp, &mut procs, t);
            s.tick(&r).unwrap();
            if r.read_count(pid, "smaps_rollup") == 2 {
                first_forced.push(t);
                break;
            }
        }
    }
    first_forced.dedup();
    assert_eq!(first_forced.len(), 3, "{first_forced:?}");
}

#[test]
fn ten_ticks_read_status_on_full_scans_and_smaps_only_for_the_selected() {
    // Criterio do C6b: 10 ticks a 1 s, memoria parada, sessao 20 selecionada.
    let dir = TempDir::new("ac-cadence-status").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    s.select(Some(20));
    let proc_of = |snap: &agentcenter_core::Snapshot, pid: u32| {
        snap.sessions
            .iter()
            .flat_map(|x| x.processes.iter())
            .find(|p| p.pid == pid)
            .cloned()
            .unwrap()
    };
    for t in 0..10u64 {
        if t == 2 {
            // Pid novo na sessao 30: status lido na hora.
            fp.add(&FakeProcess::new(50, 30, "rg")).unwrap();
        }
        if t == 3 {
            // RSS e threads mexem no stat; o RSS anda 1 pagina (4 kB), que e
            // exatamente 1/256 de 1 MiB: nao passa do limite, smaps nao e relido.
            procs[2].rss_pages = 257;
            procs[2].threads = 6;
            fp.add(&procs[2]).unwrap();
        }
        let snap = s.tick(&root).unwrap();
        let p30 = proc_of(&snap, 30);
        // Entre varreduras o status em cache continua publicado (uid 1000, nao 0).
        assert_eq!(p30.uid, 1000, "tick {t}");
        assert_eq!(p30.voluntary_ctxt_switches, 10, "tick {t}");
        // RSS e threads do stat, todo tick: nao esperam a varredura do status.
        let (rss, threads) = if t >= 3 { (257, 6) } else { (256, 4) };
        assert_eq!(p30.rss_bytes, Some(rss * 4096), "tick {t}");
        assert_eq!(p30.threads, threads, "tick {t}");
        if t >= 2 {
            assert_eq!(proc_of(&snap, 50).uid, 1000, "tick {t}");
        }
    }
    // Selecionada (20 e o filho 21): status e smaps todo tick.
    for pid in [20, 21] {
        assert_eq!(root.read_count(pid, "status"), 10, "selecionada: {pid}");
        assert_eq!(
            root.read_count(pid, "smaps_rollup"),
            10,
            "selecionada: {pid}"
        );
    }
    // Nao selecionada: status nas varreduras dos ticks 0 e 5; smaps so na
    // leitura inicial (memoria parada, longe dos 300 s).
    assert_eq!(root.read_count(30, "status"), 2, "varreduras 0 e 5");
    assert_eq!(root.read_count(30, "smaps_rollup"), 1);
    assert_eq!(
        root.read_count(50, "status"),
        2,
        "novo no tick 2 e varredura 5"
    );
    assert_eq!(root.read_count(50, "smaps_rollup"), 1);
}

#[test]
fn idle_smaps_is_not_reread_before_300_ticks_by_default() {
    // 600 % 300 == 0: o espalhamento por pid cai no mesmo tick da idade.
    assert_eq!(
        smaps_reads(600, 301, SamplerConfig::default(), |_, _| {}),
        [0, 300]
    );
    // 30 tem a vez no tick 270 (espalhamento da primeira leitura); dai em diante
    // releitura sem mudanca de memoria exatamente a cada 300 ticks.
    assert_eq!(
        smaps_reads(30, 871, SamplerConfig::default(), |_, _| {}),
        [0, 270, 570, 870]
    );
}

#[test]
fn cadence_follows_the_interval_in_ticks() {
    let dir = TempDir::new("ac-cadence-interval").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.counting_root();
    // 2,5 s por tick: 5 s de smaps viram 2 ticks.
    let mut s = Sampler::with_config(
        Registry::default(),
        SamplerConfig {
            interval: Duration::from_millis(2500),
            ..SamplerConfig::default()
        },
    );
    for i in 0..10 {
        churn(&fp, &mut procs, i);
        s.tick(&root).unwrap();
    }
    assert_eq!(root.read_count(30, "smaps_rollup"), 5);
}

#[test]
fn stat_of_every_pid_only_every_5_ticks_new_and_session_pids_every_tick() {
    let dir = TempDir::new("ac-cadence-stat").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let shell = FakeProcess::new(40, 1, "bash");
    let make = FakeProcess::new(42, 1, "make");
    fp.add(&shell).unwrap();
    fp.add(&make).unwrap();
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let owner = |snap: &agentcenter_core::Snapshot, pid: u32| {
        snap.sessions
            .iter()
            .find(|x| x.processes.iter().any(|p| p.pid == pid))
            .map(|x| x.pid)
    };
    for t in 0..10 {
        // Sessoes ativas: o stat dos seus pids e lido todo tick.
        busy(&fp, &mut procs, t);
        if t == 2 {
            // Pid novo na listagem: lido na hora, ja entra na sessao 20.
            fp.add(&FakeProcess::new(50, 20, "rg")).unwrap();
        }
        if t == 6 {
            // Fora de sessao: exec num agente e reparenting para dentro da 30.
            let mut agent = shell.clone();
            agent.comm = "claude".into();
            fp.add(&agent).unwrap();
            let mut adopted = make.clone();
            adopted.ppid = 30;
            fp.add(&adopted).unwrap();
        }
        let snap = s.tick(&root).unwrap();
        assert_eq!(
            snap.machine.processes,
            if t < 2 { 5 } else { 6 },
            "tick {t}"
        );
        if t >= 2 {
            assert_eq!(owner(&snap, 50), Some(20), "tick {t}");
        }
        // Nao relidos ate a proxima varredura completa (tick 10).
        assert!(snap.sessions.iter().all(|x| x.pid != 40), "tick {t}");
        assert_eq!(owner(&snap, 42), None, "tick {t}");
    }
    for pid in [20, 21, 30] {
        assert_eq!(root.read_count(pid, "stat"), 10, "de sessao ativa: {pid}");
    }
    assert_eq!(root.read_count(50, "stat"), 8, "novo no tick 2");
    assert_eq!(
        root.read_count(40, "stat"),
        2,
        "fora de sessao: ticks 0 e 5"
    );
    assert_eq!(root.read_count(42, "stat"), 2);

    // Varredura completa: o exec e o reparenting aparecem.
    let snap = s.tick(&root).unwrap();
    assert_eq!(owner(&snap, 40), Some(40));
    assert_eq!(owner(&snap, 42), Some(30));
    assert_eq!(root.read_count(40, "stat"), 3);
}

#[test]
fn machine_memory_load_and_psi() {
    let dir = TempDir::new("ac-machine").unwrap();
    let (fp, _) = two_sessions(&dir);
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    // Kernel sem /proc/pressure: psi None, resto presente.
    let m = s.tick(&root).unwrap().machine;
    assert_eq!(m.psi, None);
    assert_eq!(m.mem_total_bytes, Some(64 << 30));
    assert_eq!(m.mem_available_bytes, Some(32 << 30));
    assert_eq!(m.swap_total_bytes, Some(8 << 30));
    assert_eq!(m.swap_free_bytes, Some(6 << 30));
    assert_eq!(m.load_avg.map(|l| l.one), Some(1.5));

    fp.set_pressure(4.0).unwrap();
    let m = s.tick(&root).unwrap().machine;
    let psi = m.psi.expect("pressure/ presente");
    let io = psi.io.unwrap();
    assert_eq!((io.some_avg10, io.some_avg60), (Some(4.0), Some(2.0)));
    assert_eq!((io.full_avg10, io.full_avg60), (Some(2.0), Some(1.0)));
    assert!(psi.cpu.is_some() && psi.memory.is_some());
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["psi"]["io"]["some_avg10"], 4.0);

    // Um recurso so ausente: os outros continuam.
    std::fs::remove_file(fp.root().join("pressure/cpu")).unwrap();
    let psi = s.tick(&root).unwrap().machine.psi.unwrap();
    assert_eq!(psi.cpu, None);
    assert!(psi.io.is_some());
}

#[test]
fn history_per_session_follows_sessions_and_pid_reuse() {
    let dir = TempDir::new("ac-history").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    // Disco andando: a sessao 20 fica ativa e o pid reusado aparece no tick em
    // que acontece (quieta, apareceria na varredura completa seguinte).
    for i in 0..3u64 {
        fp.set_cpu(16, 100 * i, 1500 * i).unwrap();
        busy(&fp, &mut procs, i);
        s.tick(&root).unwrap();
    }
    let h = s.history().get(20).unwrap();
    assert_eq!(h.len(), 3);
    assert_eq!(h.cpu_pct.capacity(), HISTORY_LEN);
    // Primeiro tick sem anterior: NaN, nao 0.
    assert!(h.cpu_pct.to_vec()[0].is_nan());
    assert_eq!(h.cpu_pct.latest(), Some(0.0));
    assert_eq!(h.pss_bytes.latest(), Some(2.0 * 1024.0 * 1024.0));
    assert_eq!(h.stall.latest(), Some(0.0), "fallback D: threads em D");

    // Sessao 30 some: o historico vai junto.
    std::fs::remove_dir_all(fp.root().join("30")).unwrap();
    busy(&fp, &mut procs[..2], 3);
    s.tick(&root).unwrap();
    assert!(s.history().get(30).is_none());
    assert_eq!(s.history().len(), 1);

    // Pid 20 reusado por outro claude: historico recomeca.
    let mut again = FakeProcess::new(20, 1, "claude");
    again.starttime = 99_999;
    fp.add(&again).unwrap();
    s.tick(&root).unwrap();
    assert_eq!(s.history().get(20).unwrap().len(), 1);
}

#[test]
fn spawned_sampler_delivers_frames_by_channel_and_stops_on_drop() {
    let dir = TempDir::new("ac-spawn").unwrap();
    let (fp, _) = two_sessions(&dir);
    let root = fp.counting_root();
    let config = SamplerConfig {
        interval: Duration::from_millis(100),
        ..SamplerConfig::default()
    };
    let handle = Sampler::with_config(Registry::default(), config).spawn(root.clone(), true);
    handle.select(Some(30));
    let start = Instant::now();
    let frames: Vec<_> = (0..4).map(|_| handle.recv().unwrap().unwrap()).collect();
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(280), "ritmo: {elapsed:?}");
    let last = frames.last().unwrap();
    assert_eq!(last.snapshot.sessions.len(), 2);
    let h = last.history.as_ref().unwrap();
    assert!(h.get(20).unwrap().len() >= 4);
    // A selecao chegou pelo canal de controle: 30 relido a cada tick depois dela.
    assert!(root.read_count(30, "smaps_rollup") >= 3);
    assert!(root.read_count(20, "smaps_rollup") <= 1);

    handle.set_interval(Duration::from_millis(5));
    let t = Instant::now();
    for _ in 0..5 {
        handle.recv().unwrap().unwrap();
    }
    // No intervalo antigo seriam >= 400 ms (um frame pode ja estar na fila).
    assert!(t.elapsed() < Duration::from_millis(300), "intervalo novo");
    let t = Instant::now();
    drop(handle);
    assert!(t.elapsed() < Duration::from_secs(1), "drop para a thread");
}

/// Uma sessao so (`pid`, nao selecionada) com 40 MiB de RSS: o limite de 1/256
/// e 160 kB (40 paginas). `at(t, p)` mexe no processo antes do tick `t`.
/// Devolve os ticks em que o `smaps_rollup` foi lido.
fn smaps_reads(
    pid: u32,
    ticks: u64,
    config: SamplerConfig,
    mut at: impl FnMut(u64, &mut FakeProcess),
) -> Vec<u64> {
    let dir = TempDir::new("ac-cadence-moved").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(pid, 1, "claude");
    p.rss_pages = 10_240;
    let root = fp.counting_root();
    let mut s = Sampler::with_config(Registry::default(), config);
    let (mut reads, mut last) = (Vec::new(), 0);
    for t in 0..ticks {
        at(t, &mut p);
        fp.add(&p).unwrap();
        s.tick(&root).unwrap();
        let n = root.read_count(pid, "smaps_rollup");
        if n != last {
            reads.push(t);
            last = n;
        }
    }
    reads
}

#[test]
fn smaps_rereads_after_5_ticks_when_only_rss_moved() {
    // (30 + t) % 300 so zera no tick 270: aqui nenhuma releitura e por idade.
    let reads = smaps_reads(30, 12, SamplerConfig::default(), |t, p| {
        if t == 1 {
            p.rss_pages += 41;
        }
    });
    assert_eq!(reads, [0, 5]);
}

#[test]
fn smaps_rereads_after_5_ticks_when_only_swap_moved() {
    let reads = smaps_reads(30, 12, SamplerConfig::default(), |t, p| {
        if t == 1 {
            p.swap_kb = 161;
        }
    });
    assert_eq!(reads, [0, 5]);
}

#[test]
fn memory_moving_up_to_1_256_does_not_reread_smaps() {
    let reads = smaps_reads(30, 16, SamplerConfig::default(), |t, p| {
        // Disco andando: sessao ativa, o RSS do stat e visto todo tick.
        p.io = Some([4096 * (t + 1), 0, 0, 0]);
        if t == 1 {
            // Exatamente no limite (160 kB), nos dois contadores: nao passou.
            p.rss_pages += 40;
            p.swap_kb = 160;
        }
        if t == 13 {
            p.rss_pages += 1;
        }
    });
    assert_eq!(reads, [0, 13]);
}

#[test]
fn smaps_cadence_rounds_a_fractional_interval_up() {
    // 5 s a 1,5 s por tick sao 3,33 ticks: 4, nao 3.
    let config = SamplerConfig {
        interval: Duration::from_millis(1500),
        ..SamplerConfig::default()
    };
    let reads = smaps_reads(30, 10, config, |t, p| p.rss_pages = 10_240 + 100 * t);
    assert_eq!(reads, [0, 4, 8]);
}

#[test]
fn idle_smaps_is_reread_exactly_at_max_age() {
    // max_age de 10 ticks; (37 + t) % 10 zera nos ticks 3, 13, 23, sempre com
    // menos de 5 ticks de idade: quem rele e a idade, no tick 10, 20, 30.
    let config = SamplerConfig {
        smaps_max_age: Duration::from_secs(10),
        ..SamplerConfig::default()
    };
    let reads = smaps_reads(37, 31, config, |_, _| {});
    assert_eq!(reads, [0, 10, 20, 30]);
}

#[test]
fn task_of_idle_session_is_rescanned_exactly_every_5_ticks() {
    let dir = TempDir::new("ac-cadence-task").unwrap();
    let (fp, mut procs) = two_sessions(&dir);
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    for t in 0..11 {
        if t == 4 {
            procs[2].d_threads = 1;
            fp.add(&procs[2]).unwrap();
        }
        let snap = s.tick(&root).unwrap();
        let d = snap
            .sessions
            .iter()
            .find(|x| x.pid == 30)
            .unwrap()
            .threads_d;
        // Thread em D desde o tick 4; so aparece na varredura do tick 5.
        assert_eq!(d, Some(u32::from(t >= 5)), "tick {t}");
    }
    assert_eq!(root.read_count(30, "task"), 3, "ticks 0, 5 e 10");
}

#[test]
fn stall_history_is_nan_without_a_previous_tick_under_delayacct() {
    let dir = TempDir::new("ac-history-stall").unwrap();
    let (fp, _) = two_sessions(&dir);
    fp.set_delayacct(true).unwrap();
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    for i in 0..2u64 {
        fp.set_cpu(16, 100 * i, 1500 * i).unwrap();
        s.tick(&root).unwrap();
    }
    // Sem tick anterior nao ha stall_pct nem threads_d: NaN, nao 0.
    let stall = s.history().get(20).unwrap().stall.to_vec();
    assert!(stall[0].is_nan(), "{stall:?}");
    assert_eq!(stall[1], 0.0);
}

// Sessao quieta (C6c): sem CPU, disco nem stall na ultima leitura, so lida na
// varredura completa (ver o topo de sampler.rs).

fn session(snap: &agentcenter_core::Snapshot, pid: u32) -> &agentcenter_core::Session {
    snap.sessions.iter().find(|x| x.pid == pid).unwrap()
}

/// Ticks (de `ticks`) em que o `io` de `pid` foi lido, rodando `at(t)` antes de cada um.
fn io_reads_at(
    s: &mut Sampler,
    root: &agentcenter_core::ProcRoot,
    pid: u32,
    ticks: std::ops::Range<u64>,
    mut at: impl FnMut(u64),
) -> Vec<u64> {
    let mut out = Vec::new();
    for t in ticks {
        at(t);
        let before = root.read_count(pid, "io");
        s.tick(root).unwrap();
        if root.read_count(pid, "io") != before {
            out.push(t);
        }
    }
    out
}

#[test]
fn quiet_session_is_read_twice_in_10_ticks_active_and_selected_every_tick() {
    let dir = TempDir::new("ac-quiet").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    for p in [
        FakeProcess::new(20, 1, "claude"),
        FakeProcess::new(30, 1, "claude"),
        FakeProcess::new(31, 30, "node"),
    ] {
        fp.add(&p).unwrap();
    }
    let mut active = FakeProcess::new(40, 1, "claude");
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    s.select(Some(20));
    let counts = |pid| (root.read_count(pid, "stat"), root.read_count(pid, "io"));
    let mut base = Vec::new();
    // Tick 0: primeira leitura, sem anterior. Tick 1: delta zero, a 30 fica
    // quieta. A janela de 10 ticks comeca no 2: varreduras completas 5 e 10.
    for t in 0..12u64 {
        if t == 2 {
            base = [20, 30, 31, 40].map(counts).to_vec();
        }
        fp.set_cpu(16, 0, 1600 * t).unwrap();
        active.io = Some([4096 * (t + 1), 0, 0, 0]);
        fp.add(&active).unwrap();
        let snap = s.tick(&root).unwrap();
        // Sem leitura a sessao continua publicada, com as ultimas metricas.
        let q = session(&snap, 30);
        assert_eq!(q.procs, 2, "tick {t}");
        assert_eq!(q.pss_bytes, Some(2 * 1024 * 1024), "tick {t}");
        if t >= 1 {
            assert_eq!(q.cpu_pct, Some(0.0), "tick {t}");
            assert_eq!(q.read_bps, Some(0.0), "tick {t}");
        }
    }
    let delta = |i: usize, pid| {
        let (s0, i0) = base[i];
        let (s1, i1) = counts(pid);
        (s1 - s0, i1 - i0)
    };
    assert_eq!(delta(1, 30), (2, 2), "quieta: stat e io nos ticks 5 e 10");
    assert_eq!(delta(2, 31), (2, 2), "filho da quieta");
    assert_eq!(delta(0, 20), (10, 10), "selecionada, mesmo parada");
    assert_eq!(delta(3, 40), (10, 10), "ativa (disco andando)");
}

#[test]
fn new_process_in_quiet_session_is_read_when_it_appears_and_wakes_the_session() {
    let dir = TempDir::new("ac-quiet-new").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(30, 1, "claude")).unwrap();
    fp.add(&FakeProcess::new(31, 30, "node")).unwrap();
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let reads = io_reads_at(&mut s, &root, 30, 0..7, |_| {});
    assert_eq!(reads, [0, 1, 5], "quieta desde o tick 1");

    // Pid novo no tick 7 (fora da varredura): lido na hora, ja na sessao 30, e
    // a sessao inteira e relida. No 8 todos tem anterior e nada andou: quieta.
    fp.add(&FakeProcess::new(51, 30, "rg")).unwrap();
    let snap = s.tick(&root).unwrap();
    assert_eq!(session(&snap, 30).procs, 3);
    assert_eq!(root.read_count(51, "stat"), 1);
    assert_eq!(root.read_count(51, "io"), 1);
    assert_eq!(root.read_count(30, "io"), 4, "acordou no tick 7");
    assert_eq!(root.read_count(31, "io"), 4);
    assert_eq!(root.read_count(30, "stat"), 4, "stat do cache relido");
    let reads = io_reads_at(&mut s, &root, 51, 8..12, |_| {});
    assert_eq!(reads, [8, 10], "no 8 ainda acordada; 10 e varredura");
    assert_eq!(root.read_count(30, "io"), 6);
}

#[test]
fn waking_session_shows_the_mean_rate_of_the_period_without_a_false_peak() {
    let dir = TempDir::new("ac-quiet-rate").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut quiet = FakeProcess::new(30, 1, "claude");
    let mut busy = FakeProcess::new(40, 1, "claude");
    fp.add(&quiet).unwrap();
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
    // 16 nucleos, 1600 ticks de maquina por tick: 100 ticks por nucleo (1 s).
    for t in 0..=10u64 {
        fp.set_cpu(16, 0, 1600 * t).unwrap();
        busy.utime = 10 * t;
        fp.add(&busy).unwrap();
        if t == 6 {
            // A quieta trabalha 0,5 s de CPU e le 20 kB, tudo depois da
            // varredura do tick 5: so a do tick 10 ve.
            quiet.utime = 50;
            quiet.io = Some([20_480, 0, 0, 0]);
            fp.add(&quiet).unwrap();
        }
        let snap = s.tick(&root).unwrap();
        if t >= 1 {
            // Lida todo tick: 10 ticks por 100, como antes.
            assert!(close(session(&snap, 40).cpu_pct, 10.0), "tick {t}");
        }
        let q = session(&snap, 30);
        if (1..10).contains(&t) {
            assert_eq!(q.cpu_pct, Some(0.0), "tick {t}: ate 5 s de atraso");
        }
        if t == 10 {
            // 50 ticks em 5 s (desde a leitura do tick 5): 10 %, nao 50 %.
            assert!(close(q.cpu_pct, 10.0), "{:?}", q.cpu_pct);
            assert!(close(q.read_bps, 4096.0), "{:?}", q.read_bps);
        }
    }
    // CPU andando basta para a sessao ser lida todo tick.
    assert_eq!(root.read_count(40, "io"), 11);
}

#[test]
fn io_stall_without_cpu_keeps_the_session_awake() {
    // Sem delayacct: processo em D (swap-in: CPU ~0, read_bytes 0) ou thread
    // em D no task/. 50 e o controle, parado e sem stall.
    let dir = TempDir::new("ac-quiet-stall").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut swapin = FakeProcess::new(30, 1, "claude");
    swapin.state = 'D';
    let mut thread = FakeProcess::new(40, 1, "claude");
    thread.threads = 4;
    thread.d_threads = 1;
    for p in [&swapin, &thread, &FakeProcess::new(50, 1, "claude")] {
        fp.add(p).unwrap();
    }
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    for _ in 0..10 {
        s.tick(&root).unwrap();
    }
    assert_eq!(root.read_count(30, "io"), 10, "processo em D");
    assert_eq!(root.read_count(40, "io"), 10, "thread em D");
    assert_eq!(root.read_count(50, "io"), 3, "controle: 0, 1 e 5");

    // Com delayacct: stall andando em estado S, e processo em D com o
    // `delayacct_blkio_ticks` parado (o swap-in tem contador proprio no kernel).
    let dir = TempDir::new("ac-quiet-blkio").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(true).unwrap();
    let mut blk = FakeProcess::new(60, 1, "claude");
    let mut swapin = FakeProcess::new(80, 1, "claude");
    swapin.state = 'D';
    fp.add(&swapin).unwrap();
    fp.add(&FakeProcess::new(70, 1, "claude")).unwrap();
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    for t in 0..10 {
        blk.blkio_ticks = t;
        fp.add(&blk).unwrap();
        s.tick(&root).unwrap();
    }
    assert_eq!(
        root.read_count(60, "io"),
        10,
        "delayacct_blkio_ticks andando"
    );
    assert_eq!(root.read_count(80, "io"), 10, "em D, blkio parado");
    assert_eq!(root.read_count(70, "io"), 3);
}

#[test]
fn quiet_session_sees_a_reused_pid_at_the_next_full_scan() {
    let dir = TempDir::new("ac-quiet-reuse").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(30, 1, "claude")).unwrap();
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    for t in 0..=5u64 {
        if t == 2 {
            let mut again = FakeProcess::new(30, 1, "claude");
            again.starttime = 99_999;
            fp.add(&again).unwrap();
        }
        s.tick(&root).unwrap();
        // Quieta desde o tick 1: o pid reusado no 2 so aparece na varredura do 5.
        let want = if t < 5 { t as usize + 1 } else { 1 };
        assert_eq!(s.history().get(30).unwrap().len(), want, "tick {t}");
    }
}

#[test]
fn quiet_sessions_spread_forced_smaps_rereads_over_the_full_scans() {
    // 30 sessoes quietas de um processo, memoria parada. Cada pid tem a vez
    // em `(t + pid) % 300 == 0`, quase sempre fora de uma varredura; a releitura
    // cai na varredura seguinte, e nao todas juntas nos 300 s.
    let dir = TempDir::new("ac-quiet-spread").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let pids: Vec<u32> = (30..60).collect();
    for &pid in &pids {
        fp.add(&FakeProcess::new(pid, 1, "claude")).unwrap();
    }
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let mut first: Vec<Option<u64>> = vec![None; pids.len()];
    for t in 0..=300u64 {
        s.tick(&root).unwrap();
        for (i, &pid) in pids.iter().enumerate() {
            if first[i].is_none() && root.read_count(pid, "smaps_rollup") == 2 {
                first[i] = Some(t);
            }
        }
    }
    let first: Vec<u64> = first.into_iter().map(Option::unwrap).collect();
    for (&pid, &t) in pids.iter().zip(&first) {
        // A varredura seguinte a vez do pid, antes dos 300 s de idade.
        let turn = 300 - u64::from(pid);
        assert_eq!(t, turn.div_ceil(5) * 5, "pid {pid}: {first:?}");
    }
    let mut ticks = first.clone();
    ticks.sort_unstable();
    ticks.dedup();
    assert_eq!(ticks, [245, 250, 255, 260, 265, 270], "{first:?}");
}

#[test]
fn session_under_1_pct_of_a_core_summed_stays_quiet_and_shows_the_window_mean() {
    // 16 nucleos, 3200 ticks de maquina por tick: 200 ticks por nucleo, entao
    // 1 tick de CPU por tick e 0,5 % de um nucleo.
    let dir = TempDir::new("ac-quiet-threshold").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    // (processo, ticks de CPU por tick): 30 a 0,5 %, 40 a 1,5 %, 60 em 1 %
    // exato, 50 com dois processos a 0,5 % (1 % somado).
    let mut procs = [
        (FakeProcess::new(30, 1, "claude"), 1),
        (FakeProcess::new(40, 1, "claude"), 3),
        (FakeProcess::new(50, 1, "claude"), 1),
        (FakeProcess::new(51, 50, "node"), 1),
        (FakeProcess::new(60, 1, "claude"), 2),
    ];
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
    for t in 0..=10u64 {
        fp.set_cpu(16, 0, 3200 * t).unwrap();
        for (p, per_tick) in procs.iter_mut() {
            p.utime = *per_tick * t;
            fp.add(p).unwrap();
        }
        let snap = s.tick(&root).unwrap();
        if t >= 1 {
            // Sem leitura a quieta republica a media da ultima janela, nao 0.
            assert!(close(session(&snap, 30).cpu_pct, 0.5), "tick {t}");
            assert!(close(session(&snap, 40).cpu_pct, 1.5), "tick {t}");
            assert!(close(session(&snap, 50).cpu_pct, 1.0), "tick {t}");
        }
    }
    assert_eq!(
        root.read_count(30, "io"),
        4,
        "0,5 %: quieta, lida em 0, 1, 5 e 10"
    );
    assert_eq!(root.read_count(40, "io"), 11, "1,5 % acorda");
    assert_eq!(root.read_count(60, "io"), 11, "1 % nao esta abaixo de 1 %");
    assert_eq!(
        root.read_count(50, "io"),
        11,
        "a soma da sessao conta, nao o processo"
    );
    assert_eq!(root.read_count(51, "io"), 11);
}

#[test]
fn cmdline_and_cwd_on_appearance_full_scan_and_every_tick_for_the_selected() {
    let dir = TempDir::new("ac-cmdline").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let with_cwd = |pid, cwd: &str| {
        let mut p = FakeProcess::new(pid, 1, "claude");
        p.cwd = Some(cwd.to_string());
        p
    };
    fp.add(&with_cwd(20, "/w/selected")).unwrap();
    fp.add(&with_cwd(30, "/w/quiet")).unwrap();
    fp.add(&FakeProcess::new(31, 30, "node")).unwrap();
    let mut active = with_cwd(40, "/w/active");
    let root = fp.counting_root();
    let mut s = Sampler::new(Registry::default());
    s.select(Some(20));
    let reads = |pid| (root.read_count(pid, "cmdline"), root.read_count(pid, "cwd"));
    for t in 0..=10u64 {
        if t == 7 {
            // pid do agente ativo reusado fora da varredura: lido na hora.
            active = with_cwd(40, "/w/reused");
            active.starttime = 99_999;
        }
        active.io = Some([4096 * (t + 1), 0, 0, 0]);
        fp.add(&active).unwrap();
        let snap = s.tick(&root).unwrap();
        // Entre leituras o valor em cache continua publicado.
        assert_eq!(
            session(&snap, 30).cwd.as_deref(),
            Some("/w/quiet"),
            "tick {t}"
        );
        assert_eq!(session(&snap, 30).cmdline.as_deref(), Some("claude"));
        let want = if t < 7 { "/w/active" } else { "/w/reused" };
        assert_eq!(session(&snap, 40).cwd.as_deref(), Some(want), "tick {t}");
    }
    assert_eq!(reads(20), (11, 11), "selecionada: todo tick");
    assert_eq!(reads(30), (3, 3), "quieta: 0, 5 e 10");
    assert_eq!(reads(40), (4, 4), "ativa: 0, 5, 7 (pid reusado) e 10");
    assert_eq!(reads(31), (0, 0), "so o agente");
}
