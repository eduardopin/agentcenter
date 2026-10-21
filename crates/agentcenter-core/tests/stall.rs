//! Deltas entre ticks: CPU, disco, stall (delayacct e fallback D) e io_bound.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::session::{IO_BOUND_MAX_CPU_PCT, IO_BOUND_STALL_PCT, Metric, io_bound};
use agentcenter_core::{IoStallSource, Registry, Sampler, Snapshot};

/// Dois ticks: 16 nucleos, 1 s de parede = 1600 ticks de maquina (100 por nucleo).
fn two_ticks(
    delayacct: bool,
    t0: &[FakeProcess],
    t1: &[FakeProcess],
) -> (Snapshot, Snapshot, TempDir) {
    let dir = TempDir::new("ac-stall").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(delayacct).unwrap();
    fp.set_cpu(16, 0, 0).unwrap();
    for p in t0 {
        fp.add(p).unwrap();
    }
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    let a = s.tick(&root).unwrap();
    fp.set_cpu(16, 400, 1200).unwrap();
    for p in t1 {
        fp.add(p).unwrap();
    }
    let b = s.tick(&root).unwrap();
    (a, b, dir)
}

fn proc_(pid: u32, ppid: u32, comm: &str, cpu: u64, blkio: u64, read: u64) -> FakeProcess {
    let mut p = FakeProcess::new(pid, ppid, comm);
    p.utime = cpu;
    p.blkio_ticks = blkio;
    p.io = Some([read, read / 2, read * 2, read]);
    p
}

#[test]
fn cpu_and_disk_rates_from_deltas() {
    let (a, b, _d) = two_ticks(
        false,
        &[
            proc_(20, 1, "claude", 1000, 0, 0),
            proc_(21, 20, "node", 0, 0, 0),
        ],
        &[
            proc_(20, 1, "claude", 1050, 0, 1 << 20),
            proc_(21, 20, "node", 25, 0, 0),
        ],
    );
    // Primeiro tick: sem anterior, sem taxas (ausente, nao 0).
    assert_eq!(a.interval_ms, 0);
    assert_eq!(a.sessions[0].cpu_pct, None);
    assert_eq!(a.machine.cpu_pct, None);
    assert_eq!(b.interval_ms, 1000);
    let s = &b.sessions[0];
    let cpu = s.cpu_pct.unwrap();
    assert!(
        (cpu - 75.0).abs() < 1e-9,
        "50 + 25 ticks em 100 por nucleo: {cpu}"
    );
    assert!((s.cpu_share_pct.unwrap() - 75.0 / 16.0).abs() < 1e-9);
    assert!((s.read_bps.unwrap() - f64::from(1u32 << 20)).abs() < 1e-6);
    assert!((s.write_bps.unwrap() - f64::from(1u32 << 19)).abs() < 1e-6);
    assert!(s.partial.is_empty(), "todos com anterior: {:?}", s.partial);
    assert!((b.machine.cpu_pct.unwrap() - 25.0).abs() < 1e-9);
}

#[test]
fn delayacct_on_stall_from_field_42_delta() {
    let (a, b, _d) = two_ticks(
        true,
        &[
            proc_(20, 1, "claude", 0, 100, 0),
            proc_(21, 20, "bash", 0, 0, 0),
        ],
        &[
            proc_(20, 1, "claude", 2, 130, 0),
            proc_(21, 20, "bash", 0, 60, 0),
        ],
    );
    assert_eq!(a.machine.io_stall_source, IoStallSource::Delayacct);
    let s = &b.sessions[0];
    // 30 e 60 ticks de 100: o maior processo define o stall da sessao.
    assert_eq!(s.stall_pct, Some(60.0));
    assert_eq!(s.threads_d, None);
    let claude = s.processes.iter().find(|p| p.pid == 20).unwrap();
    assert_eq!(claude.stall_pct, Some(30.0));
    assert!(s.io_bound, "stall 60% com cpu 2%");
}

#[test]
fn delayacct_off_falls_back_to_d_threads_inside_sessions_only() {
    let mut busy = proc_(21, 20, "cargo", 0, 999, 0);
    busy.d_threads = 2;
    let mut stray = proc_(40, 1, "updatedb", 0, 0, 0);
    stray.d_threads = 5;
    let procs = [proc_(20, 1, "claude", 0, 0, 0), busy, stray];
    let (_a, b, _d) = two_ticks(false, &procs, &procs);
    assert_eq!(b.machine.io_stall_source, IoStallSource::DState);
    assert_eq!(b.sessions.len(), 1);
    let s = &b.sessions[0];
    assert_eq!(
        s.stall_pct, None,
        "campo 42 ignorado com delayacct desligado"
    );
    assert_eq!(s.threads_d, Some(2), "updatedb fora da sessao nao conta");
    assert!(
        !s.partial.contains(&Metric::StallPct),
        "no fallback stall_pct ausente e o normal"
    );
    assert!(s.io_bound);
}

#[test]
fn io_bound_needs_high_stall_and_low_cpu() {
    assert!(io_bound(0.0, Some(IO_BOUND_STALL_PCT), None));
    assert!(!io_bound(0.0, Some(IO_BOUND_STALL_PCT - 0.1), None));
    assert!(
        !io_bound(IO_BOUND_MAX_CPU_PCT, Some(90.0), None),
        "cpu alta: trabalhando"
    );
    assert!(io_bound(1.0, None, Some(1)));
    assert!(!io_bound(1.0, None, Some(0)));
    assert!(!io_bound(0.0, None, None));
    // Sessao com stall alto mas CPU alta nao e io_bound.
    let (_a, b, _d) = two_ticks(
        true,
        &[proc_(20, 1, "claude", 0, 0, 0)],
        &[proc_(20, 1, "claude", 50, 80, 0)],
    );
    assert_eq!(b.sessions[0].stall_pct, Some(80.0));
    assert!(!b.sessions[0].io_bound);
}

#[test]
fn reused_pid_has_no_previous_sample() {
    let mut reborn = proc_(20, 1, "claude", 5000, 0, 0);
    reborn.starttime = 999_999;
    let (_a, b, _d) = two_ticks(false, &[proc_(20, 1, "claude", 10, 0, 0)], &[reborn]);
    assert_eq!(b.sessions[0].cpu_pct, None);
    assert_eq!(b.sessions[0].processes[0].read_bps, None);
}

#[test]
fn single_thread_process_in_d_state_counts_without_task_scan() {
    let mut io = proc_(21, 20, "dd", 0, 0, 0);
    io.state = 'D';
    let procs = [proc_(20, 1, "claude", 0, 0, 0), io];
    let (a, b, _d) = two_ticks(false, &procs, &procs);
    assert_eq!(a.sessions[0].threads_d, Some(1));
    assert!(
        !a.sessions[0].io_bound,
        "primeiro tick: CPU ausente nao conta como CPU baixa"
    );
    assert_eq!(b.sessions[0].threads_d, Some(1));
    assert!(b.sessions[0].io_bound);
    let dd = b.sessions[0]
        .processes
        .iter()
        .find(|p| p.pid == 21)
        .unwrap();
    assert_eq!((dd.threads, dd.threads_d), (1, Some(1)));
}

#[test]
fn main_thread_state_comes_from_process_stat_not_task_stat() {
    let dir = TempDir::new("ac-main-thread").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(false).unwrap();
    // 21: principal em D (stat e task/21/stat concordam) e 2 threads em D.
    let mut busy = proc_(21, 20, "cargo", 0, 0, 0);
    busy.threads = 4;
    busy.d_threads = 2;
    busy.state = 'D';
    // 22: principal em S no stat do processo, mas task/22/stat diz D. Se o
    // sampler relesse a principal em task/, contaria 1 em vez de 0.
    let mut idle = proc_(22, 20, "node", 0, 0, 0);
    idle.threads = 4;
    let procs = [proc_(20, 1, "claude", 0, 0, 0), busy, idle.clone()];
    for p in &procs {
        fp.add(p).unwrap();
    }
    let mut lying = idle;
    lying.state = 'D';
    std::fs::write(fp.root().join("22/task/22/stat"), lying.stat_line()).unwrap();

    let snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let d = |pid| {
        snap.sessions[0]
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .unwrap()
            .threads_d
    };
    assert_eq!(
        d(21),
        Some(3),
        "principal do stat + 2 em D, sem contar duas vezes"
    );
    assert_eq!(d(22), Some(0), "task/<pid>/stat da principal nao e lido");
}

#[test]
fn missing_values_are_none_and_session_sum_is_partial() {
    let dir = TempDir::new("ac-missing").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(true).unwrap();
    let claude = proc_(20, 1, "claude", 100, 10, 0);
    let bash = proc_(21, 20, "bash", 0, 0, 0);
    fp.add(&claude).unwrap();
    fp.add(&bash).unwrap();
    let root = fp.proc_root();
    let mut s = Sampler::new(Registry::default());
    // Selecionada: smaps_rollup relido todo tick, entao o ilegivel do 2o tick
    // aparece ja (fora da selecao vale a cadencia, ver tests/cadence.rs).
    s.select(Some(20));

    // Primeiro tick: CPU, disco e stall sem anterior.
    let a = s.tick(&root).unwrap();
    let sa = &a.sessions[0];
    assert_eq!(
        (sa.cpu_pct, sa.cpu_share_pct, sa.read_bps, sa.stall_pct),
        (None, None, None, None)
    );
    assert!(
        sa.processes
            .iter()
            .all(|p| p.cpu_pct.is_none() && p.stall_pct.is_none())
    );
    assert_eq!(
        sa.partial,
        vec![
            Metric::CpuPct,
            Metric::ReadBps,
            Metric::WriteBps,
            Metric::StallPct
        ]
    );
    assert!(
        !sa.io_bound,
        "sem CPU medida nao da para dizer que esta parada"
    );

    // Segundo tick: 22 e novo (sem anterior) e 21 tem smaps_rollup ilegivel.
    fp.set_cpu(16, 400, 1200).unwrap();
    fp.add(&proc_(20, 1, "claude", 150, 40, 0)).unwrap();
    fp.add(&proc_(22, 20, "node", 7, 0, 0)).unwrap();
    let smaps = fp.root().join("21/smaps_rollup");
    std::fs::remove_file(&smaps).unwrap();
    std::fs::create_dir(&smaps).unwrap(); // EISDIR: le como erro, processo segue vivo
    let b = s.tick(&root).unwrap();
    let sb = &b.sessions[0];
    assert_eq!(sb.procs, 3);
    let node = sb.processes.iter().find(|p| p.pid == 22).unwrap();
    let bash = sb.processes.iter().find(|p| p.pid == 21).unwrap();
    assert_eq!(
        (node.cpu_pct, node.stall_pct, node.read_bps),
        (None, None, None)
    );
    assert_eq!((bash.pss_bytes, bash.swap_pss_bytes), (None, None));
    assert_eq!(bash.cpu_pct, Some(0.0), "0 medido continua 0");
    // Somas das medidas presentes, marcadas como parciais.
    assert_eq!(sb.cpu_pct, Some(50.0));
    assert_eq!(sb.stall_pct, Some(30.0));
    assert_eq!(sb.pss_bytes, Some(2 * 1024 * 1024), "20 e 22, 1024 kB cada");
    assert_eq!(
        sb.partial,
        vec![
            Metric::CpuPct,
            Metric::PssBytes,
            Metric::SwapPssBytes,
            Metric::ReadBps,
            Metric::WriteBps,
            Metric::StallPct
        ]
    );

    // No JSON ausente e null, nao 0.
    let v = serde_json::to_value(&b).unwrap();
    let procs = v["sessions"][0]["processes"].as_array().unwrap();
    let j_node = procs.iter().find(|p| p["pid"] == 22).unwrap();
    let j_bash = procs.iter().find(|p| p["pid"] == 21).unwrap();
    assert!(j_node["cpu_pct"].is_null() && j_node["stall_pct"].is_null());
    assert!(j_bash["pss_bytes"].is_null());
    assert_eq!(v["sessions"][0]["partial"][0], "cpu_pct");
    let va = serde_json::to_value(&a).unwrap();
    assert!(va["sessions"][0]["cpu_pct"].is_null());
    assert!(va["machine"]["cpu_pct"].is_null());
}
