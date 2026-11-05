//! Vazamento de fds: o cache de `stat`/`io` do sampler e a varredura de `task/`
//! (CC10) nao deixam descritor para tras. Um teste so neste arquivo: a contagem
//! e de `/proc/self/fd`, do processo inteiro, e outro teste em paralelo a
//! bagunçaria.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir, synthetic};
use agentcenter_core::procfs::task::count_d_threads;
use agentcenter_core::{Registry, Sampler};
use std::fs;

fn open_fds() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
fn ticks_with_churning_pids_and_task_scans_leave_no_fd_behind() {
    let dir = TempDir::new("ac-fd-leak").unwrap();
    let a = synthetic(dir.path().join("a"), 500, 1, false).unwrap();
    let b = synthetic(dir.path().join("b"), 500, 2, false).unwrap();
    let (ra, rb) = (a.proc_root(), b.proc_root());
    let before = open_fds();

    let mut s = Sampler::new(Registry::default());
    for i in 0..12 {
        s.tick(if i % 2 == 0 { &ra } else { &rb }).unwrap();
    }
    // O que a fixture de 500 processos guarda: `stat` e `io` de cada processo
    // de sessao, nas duas raizes (a alternancia do benchmark).
    let held = s.fd_cache().held();
    assert_eq!(
        held,
        2 * (2 * 374),
        "stat + io de 374 processos de sessao x 2 raizes"
    );
    assert_eq!(
        open_fds(),
        before + held,
        "so os fds do cache estao abertos"
    );

    // Grupos de pids que somem e aparecem a cada 3 ticks: as entradas dos que sairam fecham.
    let churn = FakeProc::create(dir.path().join("churn")).unwrap();
    let root = churn.proc_root();
    let mut s2 = Sampler::new(Registry::default());
    let mut peak = 0;
    for round in 0..60u32 {
        let group = (round / 3) % 3;
        let base = 1000 + 100 * group;
        churn.add(&FakeProcess::new(base, 1, "claude")).unwrap();
        for k in 1..=20 {
            churn
                .add(&FakeProcess::new(base + k, base, "bash"))
                .unwrap();
        }
        // Os outros dois grupos de pids saem.
        for other in (0..3u32).filter(|g| *g != group) {
            let ob = 1000 + 100 * other;
            for k in 0..=20 {
                let _ = fs::remove_dir_all(churn.root().join((ob + k).to_string()));
            }
        }
        s2.tick(&root).unwrap();
        peak = peak.max(s2.fd_cache().held());
    }
    assert!(
        peak > 0 && peak <= 42,
        "pico {peak}: no maximo stat+io de um grupo"
    );
    assert!(
        s2.fd_cache().held() <= 42,
        "so o grupo vivo segura fd: {}",
        s2.fd_cache().held()
    );
    drop(s2);
    drop(s);
    assert_eq!(open_fds(), before, "soltar o sampler fecha todos os fds");

    // Varredura de `task/`: abre o diretorio e uma thread por vez, e fecha.
    let mut buf = String::new();
    for _ in 0..200 {
        for pid in [3u32, 4, 8, 23] {
            count_d_threads(&ra, pid, 'S', &mut buf);
        }
    }
    count_d_threads(&ra, 99_999, 'S', &mut buf); // pid sem task/
    assert_eq!(open_fds(), before, "count_d_threads nao vaza fd");
}
