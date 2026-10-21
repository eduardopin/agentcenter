//! Parsers do leitor /proc.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::procfs::io::read_io;
use agentcenter_core::procfs::smaps::parse_smaps_rollup;
use agentcenter_core::procfs::stat::parse_stat;
use agentcenter_core::procfs::statm::parse_statm_rss;
use agentcenter_core::procfs::status::parse_status;
use agentcenter_core::procfs::system::{parse_proc_stat, read_cpu_totals};
use agentcenter_core::{ProcRoot, Registry, Sampler};
use std::path::Path;

fn fixture(name: &str) -> ProcRoot {
    ProcRoot::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/proc")
            .join(name),
    )
}

#[test]
fn stat_comm_with_spaces_and_parens_parses_by_last_paren() {
    let mut p = FakeProcess::new(4242, 7, "(a b) c)");
    p.utime = 11;
    p.stime = 22;
    p.threads = 3;
    p.starttime = 98765;
    p.blkio_ticks = 42;
    let st = parse_stat(&p.stat_line()).unwrap();
    assert_eq!(st.pid, 4242);
    assert_eq!(st.comm, "(a b) c)");
    assert_eq!(st.state, 'S');
    assert_eq!(st.ppid, 7);
    assert_eq!((st.utime, st.stime), (11, 22));
    assert_eq!(st.num_threads, 3);
    assert_eq!(st.starttime, 98765);
    assert_eq!(st.delayacct_blkio_ticks, 42);
}

#[test]
fn stat_comm_with_parens_from_static_fixture() {
    let text = std::fs::read_to_string(fixture("nested").pid_path(40, "stat")).unwrap();
    let st = parse_stat(&text).unwrap();
    assert_eq!(st.comm, "(a b) c)");
    assert_eq!(st.ppid, 1);
}

#[test]
fn stat_real_line_from_this_kernel_has_52_fields() {
    let line = "11435 (claude) S 4528 11435 4528 34827 11435 4194304 9586223 1790907437 57823 \
        477694 231457 23917 3777775 433920 20 0 21 0 2534 5793054720 113083 18446744073709551615 \
        26388480 89069872 140724996393040 0 0 0 0 4096 2072145151 0 0 0 17 3 0 0 7 0 0 89073968 \
        259117056 1009229824 140724996395441 140724996395494 140724996395494 140724996399068 0";
    let st = parse_stat(line).unwrap();
    assert_eq!(
        (st.utime, st.stime, st.num_threads, st.starttime),
        (231457, 23917, 21, 2534)
    );
    assert_eq!(st.delayacct_blkio_ticks, 7);
    assert!(parse_stat("12 (x").is_err());
    assert!(parse_stat("12 (x) S 1").is_err());
}

#[test]
fn stat_rss_is_field_24_in_pages_and_none_without_mm() {
    // Mesma linha real do teste acima: vsize (23) 5793054720, rss (24) 113083.
    let line = "11435 (claude) S 4528 11435 4528 34827 11435 4194304 9586223 1790907437 57823 \
        477694 231457 23917 3777775 433920 20 0 21 0 2534 5793054720 113083 18446744073709551615 \
        26388480 89069872 140724996393040 0 0 0 0 4096 2072145151 0 0 0 17 3 0 0 7 0 0 89073968 \
        259117056 1009229824 140724996395441 140724996395494 140724996395494 140724996399068 0";
    let st = parse_stat(line).unwrap();
    assert_eq!((st.vsize, st.rss_pages), (5_793_054_720, 113_083));
    assert_eq!(st.rss_bytes(), Some(113_083 * 4096));
    // Thread de kernel (sem mm): vsize e rss 0, RSS ausente.
    let kthread = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 11 0 0 20 0 1 0 12 0 0 \
        18446744073709551615 0 0 0 0 0 0 0 2147483647 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
    let k = parse_stat(kthread).unwrap();
    assert_eq!((k.vsize, k.rss_pages, k.rss_bytes()), (0, 0, None));
    // Tudo em swap: rss 0 com vsize > 0 e 0 bytes, nao ausente.
    let mut p = FakeProcess::new(7, 1, "node");
    p.rss_pages = 0;
    let mut f: Vec<String> = p.stat_line().split_whitespace().map(String::from).collect();
    f[22] = "4096".into();
    assert_eq!(parse_stat(&f.join(" ")).unwrap().rss_bytes(), Some(0));
}

#[test]
fn smaps_rollup_pss_and_swap_pss_kb_to_bytes() {
    let text = "00200000-7ffd ---p 00000000 00:00 0  [rollup]\nRss:  453740 kB\n\
        Pss:  388143 kB\nPss_Dirty:  375824 kB\nSwap:  40224 kB\nSwapPss:  30000 kB\nLocked: 0 kB\n";
    let r = parse_smaps_rollup(text).unwrap();
    assert_eq!(r.pss, 388_143 * 1024);
    assert_eq!(r.swap, 40_224 * 1024);
    assert_eq!(r.swap_pss, 30_000 * 1024);
    assert!(parse_smaps_rollup("Rss: 1 kB\n").is_err());
}

#[test]
fn status_and_statm_fields() {
    let s = parse_status(
        "Name:\tclaude\nUid:\t1000\t1000\t1000\t1000\nThreads:\t21\nVmRSS:\t453740 kB\n\
         voluntary_ctxt_switches:\t2971383\nnonvoluntary_ctxt_switches:\t491770\n",
    )
    .unwrap();
    assert_eq!((s.uid, s.threads), (1000, 21));
    assert_eq!(s.rss_bytes, Some(453_740 * 1024));
    assert_eq!(
        (s.voluntary_ctxt_switches, s.nonvoluntary_ctxt_switches),
        (2_971_383, 491_770)
    );
    assert_eq!(
        parse_statm_rss("1414320 113435 19479 15304 0 1392167 0").unwrap(),
        113_435 * 4096
    );
}

#[test]
fn proc_stat_totals_and_cores() {
    let t = parse_proc_stat(
        "cpu  100 5 20 1000 30 0 7 0 50 0\ncpu0 1 0 0 0 0 0 0 0 0 0\ncpu1 1 0 0 0 0 0 0 0 0 0\nintr 1\n",
    )
    .unwrap();
    // guest (50) ja esta em user: nao entra de novo.
    assert_eq!(t.total, 100 + 5 + 20 + 1000 + 30 + 7);
    assert_eq!(t.idle, 1030);
    assert_eq!(t.cores, 2);
}

#[test]
fn io_unreadable_is_none_without_error() {
    let dir = TempDir::new("ac-io").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(20, 1, "claude")).unwrap();
    let mut bash = FakeProcess::new(21, 20, "bash");
    bash.io = None;
    fp.add(&bash).unwrap();
    // `io` como diretorio: open passa e read falha (EISDIR) para qualquer usuario,
    // root incluso. Mesmo caminho de erro do EACCES de outro usuario (read_io -> None);
    // a classificacao EACCES -> Denied esta em procfs::tests.
    let io = fp.root().join("21/io");
    std::fs::create_dir(&io).unwrap();
    assert!(std::fs::read(&io).is_err(), "pre-condicao: io ilegivel");

    let root = fp.proc_root();
    let mut buf = String::new();
    assert_eq!(read_io(&root, 21, &mut buf), None);
    assert!(read_io(&root, 20, &mut buf).is_some());

    let mut s = Sampler::new(Registry::default());
    s.tick(&root).unwrap();
    fp.set_cpu(16, 100, 1500).unwrap();
    let snap = s.tick(&root).unwrap();
    let sess = &snap.sessions[0];
    let bash = sess.processes.iter().find(|p| p.pid == 21).unwrap();
    let claude = sess.processes.iter().find(|p| p.pid == 20).unwrap();
    assert_eq!(bash.read_bps, None);
    assert_eq!(claude.read_bps, Some(0.0));
    assert_eq!(sess.procs, 2);
}

#[test]
fn proc_stat_larger_than_one_read_chunk_is_read_whole() {
    // 1200 nucleos: as linhas cpuN passam de dois blocos de leitura (8 KiB cada).
    let dir = TempDir::new("ac-bigstat").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.set_cpu(1200, 10, 20).unwrap();
    let len = std::fs::metadata(fp.root().join("stat")).unwrap().len();
    assert!(len > 2 * 8192, "pre-condicao: {len} bytes");
    let t = read_cpu_totals(&fp.proc_root(), &mut String::new()).unwrap();
    assert_eq!(t.cores, 1200);

    // Arquivo de processo grande pelo caminho publico: termina inteiro, sem corte.
    let mut big = String::from("Name:\tclaude\n");
    while big.len() < 3 * 8192 {
        big.push_str("Pad:\t0123456789abcdef0123456789abcdef\n");
    }
    big.push_str("Pss:\t42 kB\n");
    std::fs::create_dir_all(fp.root().join("7")).unwrap();
    std::fs::write(fp.root().join("7/smaps_rollup"), &big).unwrap();
    let mut buf = String::new();
    fp.proc_root()
        .read_into(Path::new("7/smaps_rollup"), &mut buf)
        .unwrap();
    assert_eq!(buf, big);
    assert_eq!(parse_smaps_rollup(&buf).unwrap().pss, 42 * 1024);
}
