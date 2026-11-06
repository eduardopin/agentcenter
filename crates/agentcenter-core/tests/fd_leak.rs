//! Vazamento de fds: o cache de `stat`/`io` do sampler e a varredura de `task/`
//! (CC10) nao deixam descritor para tras, e o cache segura exatamente os fds
//! que o orcamento e o conjunto vivo permitem.
//!
//! A contagem e de `/proc/self/fd`, do processo inteiro, e outro teste em
//! paralelo a bagunçaria: todo teste deste arquivo pega `SERIAL`. Nada aqui
//! depende do `ulimit -n` da maquina: o orcamento e dado com
//! `Sampler::set_fd_budget` (valor fixo, ou derivado do limite lido de
//! `/proc/self/limits`), e o teste de EMFILE roda num filho com limite proprio.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir, synthetic};
use agentcenter_core::procfs::fdcache::FdCache;
use agentcenter_core::procfs::task::count_d_threads;
use agentcenter_core::{ProcRoot, Registry, Sampler};
use std::fs;
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn open_fds() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

/// Limite brando de fds (`Max open files`) deste processo.
fn soft_limit() -> usize {
    let limits = fs::read_to_string("/proc/self/limits").unwrap();
    let col = limits
        .lines()
        .find_map(|l| l.strip_prefix("Max open files"))
        .and_then(|r| r.split_whitespace().next())
        .unwrap();
    col.parse().unwrap_or(usize::MAX)
}

/// Troca o arquivo por outro inode de mesmo conteudo, como o `io` de um
/// processo novo no pid reusado: o fd guardado do antigo segue lendo o inode
/// velho.
fn new_inode(path: &std::path::Path) {
    let tmp = path.with_extension("novo");
    fs::write(&tmp, fs::read(path).unwrap()).unwrap();
    fs::rename(&tmp, path).unwrap();
}

/// Orcamento fixo dos testes que seguram poucos fds (no maximo 42): o padrao
/// depende do `ulimit -n` (e e 0 com limite de 256 ou menos).
const SMALL: usize = 64;

/// `stat` + `io` dos 374 processos de sessao de uma fixture de 500.
const ONE_ROOT: usize = 2 * 374;

/// Dois orcamentos, derivados do limite desta maquina para que caibam nele com
/// folga para o resto do processo (harness, diretorios temporarios, a leitura
/// transitoria de cada tick): `high` e o maior que o limite permite (ate 4096,
/// mais que as duas raizes juntas quando o limite deixa) e `low` um terco dele,
/// sempre abaixo do que as duas raizes pedem (`2 * ONE_ROOT`).
fn budgets() -> [usize; 2] {
    let high = soft_limit().saturating_sub(128).clamp(24, 4096);
    [high / 3, high]
}

#[test]
fn held_fds_equal_the_budget_or_the_demand_whichever_is_smaller() {
    let _g = serial();
    let dir = TempDir::new("ac-fd-leak-budget").unwrap();
    let a = synthetic(dir.path().join("a"), 500, 1, false).unwrap();
    let b = synthetic(dir.path().join("b"), 500, 2, false).unwrap();
    let (ra, rb) = (a.proc_root(), b.proc_root());

    for budget in budgets() {
        let before = open_fds();
        let mut s = Sampler::new(Registry::default());
        s.set_fd_budget(budget);
        for i in 0..12 {
            s.tick(if i % 2 == 0 { &ra } else { &rb }).unwrap();
        }
        // O que as duas raizes da fixture pedem: `stat` e `io` de cada
        // processo de sessao (a alternancia do benchmark). O orcamento limita.
        let held = s.fd_cache().held();
        assert_eq!(
            held,
            budget.min(2 * ONE_ROOT),
            "orcamento {budget}, limite {}: stat + io de 374 processos x 2 raizes",
            soft_limit()
        );
        assert_eq!(
            s.fd_cache().budget(),
            budget,
            "nenhum EMFILE reduziu o teto"
        );
        assert_eq!(
            open_fds(),
            before + held,
            "so os fds do cache estao abertos"
        );
        assert!(s.fd_cache().stats().hits > 0, "o cache serviu leituras");
        drop(s);
        assert_eq!(open_fds(), before, "soltar o sampler fecha todos os fds");
    }
}

/// Mundo do churn: um grupo de 21 processos (um `claude` e 20 `bash`) por vez,
/// com disco e CPU andando para que a sessao seja lida todo tick. A cada
/// `DWELL` rodadas o grupo vivo muda e os dos outros somem do `/proc`.
const DWELL: u32 = 6;
const GROUP: usize = 21;

fn churn_round(churn: &FakeProc, round: u32) {
    let group = (round / DWELL) % 3;
    let base = 1000 + 100 * group;
    let r = u64::from(round);
    churn.set_cpu(16, 100 * r, 1500 * r).unwrap();
    let mk = |pid: u32, ppid: u32, comm: &str| {
        let mut p = FakeProcess::new(pid, ppid, comm);
        p.utime = 10 * r;
        p.io = Some([4096 * r, 0, 0, 0]);
        p
    };
    churn.add(&mk(base, 1, "claude")).unwrap();
    for k in 1..=20 {
        churn.add(&mk(base + k, base, "bash")).unwrap();
    }
    for other in (0..3u32).filter(|g| *g != group) {
        let ob = 1000 + 100 * other;
        for k in 0..=20 {
            let _ = fs::remove_dir_all(churn.root().join((ob + k).to_string()));
        }
    }
}

#[test]
fn fds_held_are_exactly_two_per_live_session_pid_through_churn() {
    let _g = serial();
    let dir = TempDir::new("ac-fd-leak-churn").unwrap();
    let churn = FakeProc::create(dir.path().join("churn")).unwrap();
    let root = churn.proc_root();
    let before = open_fds();
    let mut s = Sampler::new(Registry::default());
    s.set_fd_budget(SMALL);
    let mut hits = Vec::new();
    for round in 0..12 * DWELL {
        churn_round(&churn, round);
        s.tick(&root).unwrap();
        let held = s.fd_cache().held();
        // Primeira rodada de um grupo: os pids ainda nao eram de sessao e nada
        // se guarda; os do grupo anterior ja fecharam. Nas outras, `stat` + `io`
        // de cada pid vivo da sessao, nem um a mais (um fd de pid morto
        // continuaria contando aqui, longe do teto de `2 * pids listados`).
        let live = if round % DWELL == 0 { 0 } else { GROUP };
        assert_eq!(held, 2 * live, "rodada {round}");
        assert_eq!(open_fds(), before + held, "rodada {round}: /proc/self/fd");
        hits.push(s.fd_cache().stats().hits);
    }
    // O cache nao encheu de entradas mortas: nas rodadas estaveis do fim ele
    // continua servindo (uma delas e varredura completa, que reabre), e nada
    // foi recusado nem estourou orcamento.
    let n = hits.len();
    assert!(
        hits[n - 1] >= hits[n - 5] + 2 * 2 * GROUP as u64,
        "hits parados nas ultimas rodadas: {:?}",
        &hits[n - 6..]
    );
    assert!(hits[n - 1] > hits[n - 2] || hits[n - 2] > hits[n - 3]);
    let st = s.fd_cache().stats();
    assert_eq!((st.rejected, st.over_budget), (0, 0), "{st:?}");
    drop(s);
    assert_eq!(open_fds(), before, "soltar o sampler fecha todos os fds");
}

#[test]
fn a_pid_reused_in_a_session_does_not_keep_the_old_process_fds() {
    let _g = serial();
    let dir = TempDir::new("ac-fd-leak-reuse").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_delayacct(false).unwrap();
    let root = fp.proc_root();
    let agent = FakeProcess::new(20, 1, "claude");
    let mut child = FakeProcess::new(21, 20, "bash");
    let before = open_fds();
    let mut s = Sampler::new(Registry::default());
    s.set_fd_budget(SMALL);
    let mut held = Vec::new();
    // Disco andando: a sessao e lida todo tick.
    for t in 0..4u64 {
        fp.set_cpu(16, 100 * t, 1500 * t).unwrap();
        child.io = Some([4096 * (t + 1), 0, 0, 0]);
        fp.add(&agent).unwrap();
        fp.add(&child).unwrap();
        s.tick(&root).unwrap();
        held.push(s.fd_cache().held());
        assert_eq!(open_fds(), before + s.fd_cache().held(), "tick {t}");
    }
    assert_eq!(held, [0, 4, 4, 4], "stat + io do agente e do filho");

    // Pid 21 vira outro processo (outro starttime) com `io` em arquivo novo. O
    // `stat` ainda e lido do fd guardado e recusado pelo starttime, depois
    // reaberto e guardado; o `io` e recusado e, como o processo e novo, nao e
    // guardado: o fd antigo fecha e nao fica outro no lugar.
    fp.set_cpu(16, 400, 6000).unwrap();
    let mut again = FakeProcess::new(21, 20, "bash");
    again.starttime = 5000;
    again.io = Some([1 << 30, 0, 0, 0]);
    fp.add(&again).unwrap();
    new_inode(&fp.root().join("21").join("io"));
    s.tick(&root).unwrap();
    assert_eq!(
        s.fd_cache().stats().rejected,
        2,
        "stat e io de 21 recusados"
    );
    assert_eq!(
        s.fd_cache().held(),
        3,
        "agente (stat + io) e o stat novo de 21; o io antigo fechou"
    );
    assert_eq!(open_fds(), before + 3);

    // Dois ticks depois o processo novo e conhecido e o io volta a ser guardado.
    for t in 5..7u64 {
        fp.set_cpu(16, 100 * t, 1500 * t).unwrap();
        again.io = Some([(1 << 30) + t, 0, 0, 0]);
        fp.add(&again).unwrap();
        s.tick(&root).unwrap();
    }
    assert_eq!(s.fd_cache().held(), 4);
    assert_eq!(open_fds(), before + 4);
    drop(s);
    assert_eq!(open_fds(), before);
}

#[test]
fn the_rejection_path_drops_the_old_fd_even_when_nothing_replaces_it() {
    let _g = serial();
    let dir = TempDir::new("ac-fd-leak-reject").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(7, 1, "bash")).unwrap();
    let root = fp.proc_root();
    let mut c = FdCache::with_budget(SMALL);
    let mut buf = String::new();
    let before = open_fds();
    c.begin_tick(1);
    c.read_stat(&root, 7, true, false, &mut buf).unwrap();
    assert!(c.read_io(&root, 7, 1000, true, false, &mut buf).is_some());
    assert_eq!((c.held(), open_fds()), (2, before + 2));

    // Outro processo no pid 7: `stat` reescrito no mesmo arquivo (starttime
    // novo) e `io` em arquivo novo.
    let mut p = FakeProcess::new(7, 1, "claude");
    p.starttime = 99_999;
    p.io = Some([777, 0, 0, 0]);
    fp.add(&p).unwrap();
    new_inode(&fp.root().join("7").join("io"));

    // `keep = false`: nada toma o lugar do fd recusado.
    c.begin_tick(1);
    let st = c.read_stat(&root, 7, false, false, &mut buf).unwrap();
    assert_eq!(st.starttime, 99_999, "pelo caminho, nao pelo fd velho");
    assert_eq!((c.held(), open_fds()), (1, before + 1), "stat recusado");
    let io = c.read_io(&root, 7, 99_999, false, false, &mut buf).unwrap();
    assert_eq!(io.read_bytes, 777, "io do processo novo");
    assert_eq!((c.held(), open_fds()), (0, before), "io recusado");
    assert_eq!(c.stats().rejected, 2);

    drop(c);
    assert_eq!(open_fds(), before);
}

/// Em `/proc` real: o processo acaba, o `pread` no fd guardado falha e o fd
/// fecha com a leitura, sem deixar outro no lugar.
#[test]
fn a_process_that_exits_leaves_no_cached_fd_on_the_real_proc() {
    let _g = serial();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let root = ProcRoot::default();
    let mut c = FdCache::with_budget(SMALL);
    let mut buf = String::new();
    let before = open_fds();
    c.begin_tick(1);
    let st = c.read_stat(&root, pid, true, false, &mut buf).unwrap();
    assert!(
        c.read_io(&root, pid, st.starttime, true, false, &mut buf)
            .is_some()
    );
    assert_eq!((c.held(), open_fds()), (2, before + 2));
    child.kill().unwrap();
    child.wait().unwrap();
    c.begin_tick(1);
    assert!(c.read_stat(&root, pid, true, false, &mut buf).is_err());
    assert!(
        c.read_io(&root, pid, st.starttime, true, false, &mut buf)
            .is_none()
    );
    assert_eq!((c.held(), open_fds()), (0, before), "os dois fds fecharam");
}

/// Sem fd livre: a leitura que falhou passa, o cache devolve metade e a
/// contagem de `/proc/self/fd` bate com o que ele diz segurar. Roda num filho
/// com `ulimit -n` baixo (o limite e do processo todo).
#[test]
fn after_an_emfile_the_cache_sheds_and_the_fd_count_matches_what_it_holds() {
    const CHILD: &str = "AC_FDLEAK_EMFILE_CHILD";
    const NAME: &str = "after_an_emfile_the_cache_sheds_and_the_fd_count_matches_what_it_holds";
    const LIMIT: usize = 64;
    if std::env::var_os(CHILD).is_none() {
        let _g = serial();
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "ulimit -n {LIMIT} && exec \"$0\" --exact {NAME} --test-threads=1 --nocapture"
            ))
            .arg(exe)
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "filho falhou:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    assert!(soft_limit() <= LIMIT, "o filho tem o limite baixo");
    let dir = TempDir::new("ac-fd-leak-emfile").unwrap();
    let pids: Vec<u32> = (10..110).collect();
    let fp = FakeProc::create(dir.path()).unwrap();
    for &pid in &pids {
        fp.add(&FakeProcess::new(pid, 1, "bash")).unwrap();
    }
    let root: ProcRoot = fp.proc_root();
    let before = open_fds();
    let mut c = FdCache::with_budget(10_000);
    let mut buf = String::new();
    for tick in 0..3 {
        c.begin_tick(pids.len());
        for &pid in &pids {
            let st = c.read_stat(&root, pid, true, false, &mut buf);
            assert_eq!(st.unwrap().pid, pid, "tick {tick}: leitura {pid} passa");
        }
        // Sem o desvio de EMFILE as leituras acima falhariam; com ele o cache
        // fica abaixo do limite, e cada fd que diz segurar esta aberto.
        assert!(c.held() > 0 && c.held() < LIMIT, "held {}", c.held());
        assert!(c.budget() < LIMIT, "budget {}", c.budget());
        assert_eq!(open_fds(), before + c.held(), "tick {tick}");
    }
    assert!(
        c.stats().kept > c.held() as u64,
        "alguns fds foram devolvidos"
    );
    drop(c);
    assert_eq!(open_fds(), before);
}

#[test]
fn task_scans_leave_no_fd_behind() {
    let _g = serial();
    let dir = TempDir::new("ac-fd-leak-task").unwrap();
    let a = synthetic(dir.path().join("a"), 500, 1, false).unwrap();
    let ra = a.proc_root();
    let before = open_fds();
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
