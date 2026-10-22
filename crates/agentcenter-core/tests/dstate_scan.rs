//! Varredura de `task/` (fallback de stall sem delayacct, CC10): a contagem de
//! threads em D do `count_d_threads` com `getdents64` + `openat` relativo tem de
//! ser a mesma da implementacao anterior (`read_dir` + caminho inteiro), que
//! fica aqui como oraculo.

use agentcenter_core::ProcRoot;
use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir, synthetic};
use agentcenter_core::procfs::stat::parse_stat;
use agentcenter_core::procfs::task::count_d_threads;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::Path;

/// Conta as alocacoes da thread corrente: a varredura nao pode alocar por
/// thread lida (o custo que o CC10 tirou), e o contador por thread nao se
/// mistura com os outros testes rodando em paralelo.
struct CountingAlloc;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: repassa tudo ao `System`; so incrementa um contador por thread
// (TLS `const` sem destrutor, que nao aloca).
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|n| n.set(n.get() + 1));
        // SAFETY: mesmo contrato do chamador.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: mesmo contrato do chamador.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.with(|n| n.set(n.get() + 1));
        // SAFETY: mesmo contrato do chamador.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Alocacoes feitas por `f` nesta thread.
fn allocs_in(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

/// A implementacao anterior ao CC10, como era (leitura lossy como a do
/// `read_file_into`).
fn old_count(root: &ProcRoot, pid: u32, main_state: char) -> u32 {
    let Ok(dir) = fs::read_dir(root.pid_path(pid, "task")) else {
        return 0;
    };
    let mut n = u32::from(main_state == 'D');
    for entry in dir.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            == Some(pid)
        {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path().join("stat")) else {
            continue;
        };
        let buf = String::from_utf8_lossy(&bytes);
        let state = buf
            .rfind(')')
            .and_then(|i| buf[i + 1..].split_ascii_whitespace().next());
        if state == Some("D") {
            n += 1;
        }
    }
    n
}

fn new_count(root: &ProcRoot, pid: u32, main_state: char) -> u32 {
    let mut buf = String::new();
    count_d_threads(root, pid, main_state, &mut buf)
}

/// Linha de `stat` de uma thread no estado dado.
fn thread_stat(tid: u32, state: char) -> String {
    let mut t = FakeProcess::new(tid, 1, "worker");
    t.state = state;
    t.stat_line()
}

fn write_thread(task: &Path, name: &str, state: char) {
    let dir = task.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("stat"), thread_stat(1, state)).unwrap();
}

#[test]
fn synthetic_500_counts_match_old_implementation() {
    let dir = TempDir::new("ac-dscan-syn").unwrap();
    let fp = synthetic(dir.path().join("p"), 500, 1, false).unwrap();
    let root = fp.proc_root();
    // A fixture do bench tem todas as threads em S: um terco passa a D para a
    // comparacao nao ser entre zeros.
    for pid in root.pids().unwrap() {
        let Ok(tids) = fs::read_dir(root.pid_path(pid, "task")) else {
            continue;
        };
        for e in tids.flatten() {
            let tid: u32 = e.file_name().to_str().unwrap().parse().unwrap();
            if tid != pid && tid.is_multiple_of(3) {
                fs::write(e.path().join("stat"), thread_stat(tid, 'D')).unwrap();
            }
        }
    }
    let mut scanned = 0;
    let mut total_d = 0;
    for pid in root.pids().unwrap() {
        let st = parse_stat(&fs::read_to_string(root.pid_path(pid, "stat")).unwrap()).unwrap();
        // Os dois estados da principal: o lider vem do stat do processo, nao de task/.
        for main in [st.state, 'D', 'S'] {
            let want = old_count(&root, pid, main);
            assert_eq!(new_count(&root, pid, main), want, "pid {pid} main {main}");
        }
        total_d += old_count(&root, pid, 'S');
        scanned += 1;
    }
    assert_eq!(scanned, 500);
    // A fixture tem threads em D fora da principal: a comparacao nao e entre zeros.
    assert!(total_d > 0, "{total_d}");
}

#[test]
fn leader_state_comes_from_process_stat_not_task() {
    let dir = TempDir::new("ac-dscan-leader").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(40, 1, "node");
    p.threads = 4;
    p.d_threads = 2;
    fp.add(&p).unwrap();
    let root = fp.proc_root();
    let task = root.pid_path(40, "task");
    // task/40/stat diz D, mas o estado da principal e o do stat ja lido no tick.
    fs::write(task.join("40/stat"), thread_stat(40, 'D')).unwrap();
    assert_eq!(new_count(&root, 40, 'S'), 2);
    assert_eq!(new_count(&root, 40, 'D'), 3);
    // E o inverso: task/40/stat em S nao apaga a principal em D.
    fs::write(task.join("40/stat"), thread_stat(40, 'S')).unwrap();
    assert_eq!(new_count(&root, 40, 'D'), 3);
    for main in ['S', 'D'] {
        assert_eq!(new_count(&root, 40, main), old_count(&root, 40, main));
    }
}

#[test]
fn dot_entries_do_not_count_the_process_twice() {
    // getdents64 entrega `.` e `..`; `../stat` e o stat do processo. Com a
    // principal em D (no stat do processo e em task/), so ela conta, uma vez.
    let dir = TempDir::new("ac-dscan-dots").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(50, 1, "node");
    p.state = 'D';
    p.threads = 3;
    fp.add(&p).unwrap();
    let root = fp.proc_root();
    // `./stat` (task/stat) tambem existe e esta em D: nao pode contar.
    fs::write(root.pid_path(50, "task").join("stat"), thread_stat(50, 'D')).unwrap();
    assert_eq!(new_count(&root, 50, 'D'), 1);
    assert_eq!(old_count(&root, 50, 'D'), 1);
}

#[test]
fn vanished_thread_and_odd_entries_match_old_implementation() {
    let dir = TempDir::new("ac-dscan-odd").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(60, 1, "node");
    p.threads = 6;
    p.d_threads = 3;
    fp.add(&p).unwrap();
    let root = fp.proc_root();
    let task = root.pid_path(60, "task");
    // Thread em D que terminou entre a listagem e a leitura: o diretorio
    // ficou, o stat nao.
    fs::remove_file(task.join("60001/stat")).unwrap();
    // Thread que sumiu de vez.
    fs::remove_dir_all(task.join("60002")).unwrap();
    // Arquivo solto em task/ (nao diretorio): ignorado.
    fs::write(task.join("60009"), thread_stat(60009, 'D')).unwrap();
    // Nome nao numerico: a implementacao anterior lia o stat dele igual a
    // qualquer outro (nao filtrava por nome); a nova faz o mesmo.
    write_thread(&task, "nao-numerico", 'D');
    // Nome numerico com sinal que o parse de u32 aceita como o pid: pulado
    // como a principal nas duas.
    write_thread(&task, "+60", 'D');
    // stat sem ')' ou truncado: sem estado, nao conta.
    fs::create_dir_all(task.join("60010")).unwrap();
    fs::write(task.join("60010/stat"), "60010 (worker D").unwrap();
    // comm com ')' e espaco: o estado vem depois do ULTIMO ')'.
    let mut odd = FakeProcess::new(60011, 1, "a) D (b");
    odd.state = 'D';
    fs::create_dir_all(task.join("60011")).unwrap();
    fs::write(task.join("60011/stat"), odd.stat_line()).unwrap();

    for main in ['S', 'D'] {
        let want = old_count(&root, 60, main);
        assert_eq!(new_count(&root, 60, main), want, "main {main}");
    }
    // 60003 em D, nao-numerico, 60011 (comm com ')'); 60001 e 60002 sumiram.
    assert_eq!(new_count(&root, 60, 'S'), 3);
}

#[test]
fn vanished_process_counts_zero() {
    let dir = TempDir::new("ac-dscan-gone").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(70, 1, "node");
    p.threads = 3;
    p.d_threads = 2;
    fp.add(&p).unwrap();
    let root = fp.proc_root();
    assert_eq!(new_count(&root, 70, 'D'), 3);
    // Processo que sumiu depois do stat do tick: sem task/, 0 mesmo com a
    // principal em D (como antes).
    fs::remove_dir_all(root.pid_path(70, "task")).unwrap();
    assert_eq!(new_count(&root, 70, 'D'), 0);
    assert_eq!(old_count(&root, 70, 'D'), 0);
    // task/ que nao e diretorio: nada a listar, 0.
    fs::write(root.pid_path(70, "task"), "x").unwrap();
    assert_eq!(new_count(&root, 70, 'D'), 0);
    assert_eq!(old_count(&root, 70, 'D'), 0);
    // Pid que nunca existiu.
    assert_eq!(new_count(&root, 71, 'D'), 0);
}

#[test]
fn more_threads_than_one_getdents_call_holds() {
    // 1200 entradas de ~24-32 bytes passam de qualquer buffer de 4 KB a 32 KB:
    // a lista vem em varias chamadas e nenhuma pode ser perdida.
    let dir = TempDir::new("ac-dscan-many").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(80, 1, "node");
    p.threads = 1200;
    p.d_threads = 457;
    fp.add(&p).unwrap();
    let root = fp.proc_root();
    assert_eq!(new_count(&root, 80, 'S'), 457);
    assert_eq!(new_count(&root, 80, 'D'), 458);
    assert_eq!(old_count(&root, 80, 'S'), 457);
    // Contagem por nome longo (mais bytes por entrada) tambem atravessa chamadas.
    let task = root.pid_path(80, "task");
    for i in 0..300 {
        write_thread(&task, &format!("{}{i:06}", "9".repeat(200)), 'D');
    }
    assert_eq!(new_count(&root, 80, 'S'), 757);
    assert_eq!(old_count(&root, 80, 'S'), 757);
}

#[test]
fn counting_root_still_notes_the_task_scan() {
    let dir = TempDir::new("ac-dscan-count").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut p = FakeProcess::new(90, 1, "node");
    p.threads = 2;
    fp.add(&p).unwrap();
    let root = fp.counting_root();
    let mut buf = String::new();
    count_d_threads(&root, 90, 'S', &mut buf);
    count_d_threads(&root, 91, 'S', &mut buf);
    assert_eq!(root.read_count(90, "task"), 1);
    assert_eq!(
        root.read_count(91, "task"),
        1,
        "conta a tentativa mesmo sem task/"
    );
}

#[test]
fn task_scan_allocations_do_not_grow_with_threads() {
    // A implementacao anterior alocava por thread (DirEntry, nome, dois
    // PathBuf): ~4 alocacoes x 1200. A atual nao aloca por thread.
    let dir = TempDir::new("ac-dscan-alloc").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    for (pid, threads) in [(100, 10), (101, 1200)] {
        let mut p = FakeProcess::new(pid, 1, "node");
        p.threads = threads;
        p.d_threads = threads / 3;
        fp.add(&p).unwrap();
    }
    let root = fp.proc_root();
    let mut buf = String::with_capacity(1024);
    // Aquece o bloco de leitura da thread (alocado uma vez).
    assert_eq!(count_d_threads(&root, 100, 'S', &mut buf), 3);
    let mut n = 0;
    let small = allocs_in(|| n = count_d_threads(&root, 100, 'S', &mut buf));
    assert_eq!(n, 3);
    let big = allocs_in(|| n = count_d_threads(&root, 101, 'S', &mut buf));
    assert_eq!(n, 400);
    assert_eq!(big, small, "alocacoes com 1200 threads vs 10");
}

#[test]
fn read_pid_does_not_allocate_per_read() {
    // O caminho <raiz>/<pid>/<file> vai na pilha e a leitura reaproveita o
    // `buf`: 100 leituras alocam o mesmo que 1 (antes, PathBuf + to_string
    // em cada uma).
    let dir = TempDir::new("ac-dscan-readpid").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    fp.add(&FakeProcess::new(110, 1, "node")).unwrap();
    let root = fp.proc_root();
    let mut buf = String::with_capacity(4096);
    root.read_pid(110, "stat", &mut buf).unwrap();
    let one = allocs_in(|| root.read_pid(110, "status", &mut buf).unwrap());
    let hundred = allocs_in(|| {
        for _ in 0..100 {
            root.read_pid(110, "status", &mut buf).unwrap();
        }
    });
    assert!(buf.starts_with("Name:\tnode\n"), "{buf}");
    assert_eq!(hundred, one, "alocacoes em 100 leituras vs 1");
}
