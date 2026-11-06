//! Cache de fds de `stat` e `io` (procfs::fdcache): o sampler com o cache tem
//! de devolver, tick a tick, o mesmo snapshot do sampler sem ele (orcamento 0),
//! e um pid reusado nao herda o `io` do processo anterior.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::{Registry, Sampler, Snapshot};
use std::fs;

/// Tres sessoes claude (20, 40, 60) com 6 filhos cada, 10 processos soltos e o
/// init sem `io`.
fn world() -> Vec<FakeProcess> {
    let mut v = vec![{
        let mut init = FakeProcess::new(1, 0, "systemd");
        init.io = None;
        init
    }];
    for base in [20, 40, 60] {
        let mut agent = FakeProcess::new(base, 1, "claude");
        agent.threads = 5;
        v.push(agent);
        for k in 1..=6 {
            let comm = if k <= 2 { "bash" } else { "node" };
            let mut p = FakeProcess::new(base + k, base, comm);
            p.threads = if k <= 2 { 1 } else { 4 };
            v.push(p);
        }
    }
    for pid in 100..110 {
        v.push(FakeProcess::new(pid, 1, "kworker/u32:1"));
    }
    v
}

/// Avanca os contadores de cada processo ao tick `t`; de 10 a 13 tudo para
/// (sessoes quietas: `stat` do cache e `stale`).
fn advance(procs: &mut [FakeProcess], t: u64) {
    if (10..=13).contains(&t) {
        return;
    }
    for p in procs.iter_mut() {
        let pid = u64::from(p.pid);
        p.utime += pid % 5 + t;
        p.stime += t % 3;
        p.rss_pages = 256 + (pid * t) % 97;
        p.state = if (pid + t).is_multiple_of(11) {
            'D'
        } else {
            'S'
        };
        if let Some(io) = p.io.as_mut() {
            io[0] += 4096 * (pid % 4);
            io[1] += 512 * t;
            io[2] += 100 * pid;
            io[3] += 7;
        }
    }
}

/// Orcamento fixo dos samplers com cache. O padrao sai do limite de fds da
/// maquina e vale 0 com `ulimit -n 256`: o cache desligaria e os testes
/// abaixo passariam a medir o ambiente. 128 cobre com folga `stat` e `io` dos
/// ~25 processos de sessao de [`world`].
const TEST_BUDGET: usize = 128;

fn zero_clock(mut s: Snapshot) -> Snapshot {
    s.taken_at_ms = 0;
    s
}

#[test]
fn snapshots_match_the_uncached_sampler_through_churn_exits_and_pid_reuse() {
    let dir = TempDir::new("ac-fd-diff").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(false).unwrap();
    let root = fp.proc_root();
    let mut procs = world();
    let mut cached = Sampler::new(Registry::default());
    cached.set_fd_budget(TEST_BUDGET);
    let mut plain = Sampler::new(Registry::default());
    plain.set_fd_budget(0);
    for t in 1..=24u64 {
        match t {
            // Processo sai.
            4 => {
                procs.retain(|p| p.pid != 22);
                fs::remove_dir_all(fp.root().join("22")).unwrap();
            }
            // Processo novo numa sessao existente, e uma sessao nova.
            6 => procs.push(FakeProcess::new(27, 20, "node")),
            // Pid reusado por outro processo: outro starttime, outro comm.
            8 => {
                let p = procs.iter_mut().find(|p| p.pid == 41).unwrap();
                p.starttime = 5000;
                p.comm = "node".into();
                p.utime = 0;
                p.io = Some([0; 4]);
            }
            // O agente de uma sessao sai e deixa os filhos.
            9 => {
                procs.retain(|p| p.pid != 60);
                fs::remove_dir_all(fp.root().join("60")).unwrap();
            }
            15 => procs.push(FakeProcess::new(80, 1, "claude")),
            // Pid 22 volta como outro processo (apareceu depois de sumir).
            17 => {
                let mut p = FakeProcess::new(22, 20, "bash");
                p.starttime = 7000;
                procs.push(p);
            }
            _ => {}
        }
        advance(&mut procs, t);
        fp.set_cpu(16, 200 * t, 1400 * t).unwrap();
        for p in &procs {
            fp.add(p).unwrap();
        }
        let a = zero_clock(cached.tick(&root).unwrap());
        let b = zero_clock(plain.tick(&root).unwrap());
        assert_eq!(a, b, "tick {t}");
    }
    let st = cached.fd_cache().stats();
    assert!(st.hits > 300, "o cache tem de ter servido leituras: {st:?}");
    assert!(st.rejected >= 2, "pid 41 reusado derruba stat e io: {st:?}");
    assert!(cached.fd_cache().held() > 0);
    assert_eq!(plain.fd_cache().held(), 0, "orcamento 0 desliga o cache");
    assert_eq!(plain.fd_cache().stats().hits, 0);
}

/// Troca `io` por um arquivo novo (outro inode), como faz o processo novo no
/// pid reusado: o inode antigo continua legivel por quem o guardou aberto.
fn new_io_file(fp: &FakeProc, pid: u32, io: [u64; 4]) {
    let path = fp.root().join(pid.to_string()).join("io");
    let [r, w, rc, wc] = io;
    let text = format!(
        "rchar: {rc}\nwchar: {wc}\nsyscr: 0\nsyscw: 0\nread_bytes: {r}\n\
         write_bytes: {w}\ncancelled_write_bytes: 0\n"
    );
    let tmp = path.with_extension("novo");
    fs::write(&tmp, text).unwrap();
    fs::rename(&tmp, &path).unwrap();
}

fn proc_of(snap: &Snapshot, pid: u32) -> agentcenter_core::session::ProcessMetrics {
    snap.sessions
        .iter()
        .flat_map(|s| s.processes.iter())
        .find(|p| p.pid == pid)
        .unwrap_or_else(|| panic!("pid {pid} fora do snapshot"))
        .clone()
}

#[test]
fn reused_pid_reads_its_own_io_not_the_old_process_through_a_kept_fd() {
    let dir = TempDir::new("ac-fd-reuse-io").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(false).unwrap();
    let root = fp.proc_root();
    let agent = FakeProcess::new(20, 1, "claude");
    let mut child = FakeProcess::new(21, 20, "bash");
    let mut s = Sampler::new(Registry::default());
    s.set_fd_budget(TEST_BUDGET);
    // Disco andando: a sessao e lida todo tick.
    for t in 0..3u64 {
        fp.set_cpu(16, 100 * t, 1500 * t).unwrap();
        child.io = Some([4096 * (t + 1), 0, 0, 0]);
        fp.add(&agent).unwrap();
        fp.add(&child).unwrap();
        s.tick(&root).unwrap();
    }
    assert!(s.fd_cache().held() >= 2, "stat e io do pid 21 guardados");

    // Tick 3 (sem varredura completa): o pid 21 e outro processo, com `stat`
    // reescrito e um `io` em arquivo novo; o fd de `io` guardado ainda le o
    // processo antigo (4096 * 3).
    fp.set_cpu(16, 300, 4500).unwrap();
    let mut again = FakeProcess::new(21, 20, "bash");
    again.starttime = 5000;
    fp.add(&again).unwrap();
    new_io_file(&fp, 21, [1 << 30, 0, 0, 0]);
    let snap = s.tick(&root).unwrap();
    let p = proc_of(&snap, 21);
    assert_eq!(p.cpu_pct, None, "processo novo: sem anterior");
    assert_eq!(p.read_bps, None, "processo novo: sem taxa de disco");

    // Tick 4: o processo novo leu mais 1 MiB. A taxa so existe se o `io` lido no
    // tick 3 foi o dele (1 GiB); lido do fd antigo, o delta daria 0 ou negativo.
    fp.set_cpu(16, 400, 6000).unwrap();
    again.io = Some([(1 << 30) + (1 << 20), 0, 0, 0]);
    fp.add(&again).unwrap();
    let snap = s.tick(&root).unwrap();
    let p = proc_of(&snap, 21);
    let rate = p.read_bps.expect("taxa do processo novo");
    assert!(
        (rate - f64::from(1u32 << 20)).abs() < 1e-6,
        "1 MiB em 1 s, veio {rate}"
    );
}
