//! `/proc/<pid>/task/*/stat`: fallback de stall quando `task_delayacct=0`.
//!
//! E o caminho mais caro do tick sem delay accounting: um `open` + `read` +
//! `close` por thread das sessoes. O perfil do CC10 mostrou ~72% do tempo do
//! tick completo em syscalls, entao a varredura usa um fd do `task/` e abre
//! cada `<tid>/stat` relativo a ele (`openat`), listando com `getdents64` num
//! buffer da pilha: sem `PathBuf`, `DirEntry` nem nome alocado por thread, e o
//! kernel resolve so o trecho `<tid>/stat` em vez do caminho inteiro.

use super::{ProcRoot, read_open_into};
use std::ffi::{c_char, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;

unsafe extern "C" {
    /// `openat(2)`. Variadica (o `mode` so conta com `O_CREAT`, que nao usamos).
    fn openat(dirfd: i32, path: *const c_char, flags: i32, ...) -> i32;
    /// `getdents64(2)` (glibc >= 2.30): ate `count` bytes de `linux_dirent64`.
    fn getdents64(fd: i32, dirp: *mut c_void, count: usize) -> isize;
}

const O_RDONLY: i32 = 0;
// O_CLOEXEC: 0o2000000 em x86, arm, aarch64, riscv, powerpc, s390 e mips; sparc difere.
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const O_CLOEXEC: i32 = 0x0040_0000;
#[cfg(not(any(target_arch = "sparc", target_arch = "sparc64")))]
const O_CLOEXEC: i32 = 0o2000000;

// O_DIRECTORY: 0o200000 on x86, riscv, s390, mips; 0o40000 on arm, aarch64, powerpc;
// 0x10000 on sparc. With it `open` fails with ENOTDIR on a FIFO or a device
// instead of waiting for a peer.
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const O_DIRECTORY: i32 = 0x1_0000;
#[cfg(any(
    target_arch = "arm",
    target_arch = "aarch64",
    target_arch = "powerpc",
    target_arch = "powerpc64"
))]
const O_DIRECTORY: i32 = 0o40000;
#[cfg(not(any(
    target_arch = "sparc",
    target_arch = "sparc64",
    target_arch = "arm",
    target_arch = "aarch64",
    target_arch = "powerpc",
    target_arch = "powerpc64"
)))]
const O_DIRECTORY: i32 = 0o200000;

/// Buffer do `getdents64`. Cada entrada de tid ocupa 24 a 32 bytes, entao uma
/// chamada traz ~150 threads; processos maiores pedem mais voltas.
const DENTS_BUF: usize = 4096;

/// Deslocamentos em `struct linux_dirent64 { u64 d_ino; i64 d_off;
/// u16 d_reclen; u8 d_type; char d_name[]; }` (layout fixo da ABI do kernel).
/// The kernel writes `u64` fields at the start of each entry, so the buffer is
/// aligned for them on purpose and not by luck of the stack layout.
#[repr(C, align(8))]
struct DentsBuf([u8; DENTS_BUF]);

impl DentsBuf {
    fn new() -> Self {
        Self([0; DENTS_BUF])
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

const RECLEN_AT: usize = 16;
const NAME_AT: usize = 19;

/// Conta as threads do processo em estado `D` (sono nao interrompivel, quase
/// sempre E/S). Thread que some no meio da contagem e ignorada.
///
/// `main_state` e o estado do `/proc/<pid>/stat` ja lido no tick: e o da thread
/// principal (o kernel le o lider do grupo), entao `task/<pid>/stat` nao e relido.
/// Sem `task/` legivel (processo que sumiu) a contagem e 0.
pub fn count_d_threads(root: &ProcRoot, pid: u32, main_state: char, buf: &mut String) -> u32 {
    d_threads_besides_main(root, pid, buf).map_or(0, |n| n + u32::from(main_state == 'D'))
}

/// Threads em `D` sem contar a principal, ou `None` se `task/` nao abre (processo
/// que sumiu) ou nao lista. E o que [`count_d_threads`] soma ao estado do `stat`,
/// separado para que a varredura de varios processos rode em paralelo
/// ([`scan_d_threads`]) antes de o `stat` de cada um estar fechado.
pub fn d_threads_besides_main(root: &ProcRoot, pid: u32, buf: &mut String) -> Option<u32> {
    root.note_read(pid, "task");
    // `O_DIRECTORY`: a `task` that is a FIFO must fail here, not block the tick.
    // std adds O_CLOEXEC.
    let dir = root
        .with_pid_path(pid, "task", |p| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(O_DIRECTORY)
                .open(p)
        })
        .ok()?;
    let dfd = dir.as_raw_fd();
    let mut n = 0u32;
    let mut dents = DentsBuf::new();
    // "<nome>/stat\0": d_name tem no maximo 255 bytes.
    let mut rel = [0u8; 256 + 6];
    let mut first = true;
    loop {
        // SAFETY: `dfd` e o fd de `dir`, vivo ate o fim da funcao; o kernel
        // escreve no maximo `DENTS_BUF` bytes em `dents`, que e nosso e mutavel.
        let got = unsafe { getdents64(dfd, dents.0.as_mut_ptr().cast(), DENTS_BUF) };
        if got < 0 && first {
            // `task` que abre mas nao lista (nao e diretorio): como o antigo
            // `read_dir`, que falhava ja na abertura, nada a contar.
            return None;
        }
        first = false;
        if got <= 0 {
            break;
        }
        // O kernel so devolve entradas inteiras: `got` <= DENTS_BUF e cada
        // `d_reclen` cobre a entrada toda, com `d_name` terminado em NUL.
        let filled = &dents.as_bytes()[..got as usize];
        let mut off = 0;
        while off + NAME_AT < filled.len() {
            let reclen = usize::from(u16::from_ne_bytes([
                filled[off + RECLEN_AT],
                filled[off + RECLEN_AT + 1],
            ]));
            let Some(entry) = filled.get(off..off + reclen).filter(|_| reclen > NAME_AT) else {
                break;
            };
            off += reclen;
            let raw = &entry[NAME_AT..];
            let name = &raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())];
            // `read_dir` pulava `.` e `..`; aqui o kernel os entrega. `../stat`
            // seria o stat do processo e contaria a principal duas vezes.
            if name == b"." || name == b".." || name.len() > 255 {
                continue;
            }
            if std::str::from_utf8(name)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                == Some(pid)
            {
                continue;
            }
            rel[..name.len()].copy_from_slice(name);
            rel[name.len()..name.len() + 6].copy_from_slice(b"/stat\0");
            // SAFETY: `rel` termina em NUL logo depois de "<nome>/stat" (o nome
            // nao tem NUL interno: cortamos no primeiro) e vive durante a chamada;
            // `dfd` segue aberto.
            let fd = unsafe { openat(dfd, rel.as_ptr().cast(), O_RDONLY | O_CLOEXEC) };
            if fd < 0 {
                // Thread que terminou entre a listagem e a abertura.
                continue;
            }
            // SAFETY: `fd` acabou de ser aberto por nos e nao tem outro dono;
            // o `File` passa a ser o unico e o fecha no drop.
            let mut f = unsafe { File::from_raw_fd(fd) };
            if read_open_into(&mut f, buf).is_err() {
                continue;
            }
            let state = buf
                .rfind(')')
                .and_then(|i| buf[i + 1..].split_ascii_whitespace().next());
            if state == Some("D") {
                n += 1;
            }
        }
    }
    Some(n)
}

/// Menos varreduras que isto rodam na thread de quem chama: criar thread custa
/// mais que as ~60 syscalls de um processo de poucas threads.
pub const PARALLEL_MIN_JOBS: usize = 24;

/// Threads de varredura no maximo, contando a de quem chama. A varredura e
/// quase so syscall (`openat`, `read`, `close` por thread de cada processo), que
/// o kernel atende em paralelo; mais de 4 nao paga o custo de criar as threads
/// em maquina de 4 vCPU.
pub const MAX_SCAN_WORKERS: usize = 4;

fn cpus() -> usize {
    // `available_parallelism` le o cgroup a cada chamada: uma vez so.
    static CPUS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CPUS.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// Quantas threads (contando a de quem chama) varrem `jobs` processos numa
/// maquina de `cpus` CPUs: 1 abaixo de [`PARALLEL_MIN_JOBS`], nunca mais que
/// [`MAX_SCAN_WORKERS`] nem que `cpus`, e cada thread com ao menos metade de
/// [`PARALLEL_MIN_JOBS`] processos.
pub fn scan_worker_count(jobs: usize, cpus: usize) -> usize {
    if jobs < PARALLEL_MIN_JOBS {
        return 1;
    }
    cpus.min(MAX_SCAN_WORKERS)
        .min(jobs / (PARALLEL_MIN_JOBS / 2))
        .max(1)
}

/// [`d_threads_besides_main`] de cada pid de `pids`, na mesma ordem. Com
/// trabalho suficiente divide os pids entre threads (cada uma com o seu
/// buffer, ver [`scan_worker_count`]); o resultado nao depende de como foram
/// divididos.
pub fn scan_d_threads(root: &ProcRoot, pids: &[u32]) -> Vec<Option<u32>> {
    scan_d_threads_with(root, pids, scan_worker_count(pids.len(), cpus()))
}

/// [`scan_d_threads`] com `workers` threads (o chamador conta como uma), para
/// que os testes forcem a divisao em qualquer maquina.
pub fn scan_d_threads_with(root: &ProcRoot, pids: &[u32], workers: usize) -> Vec<Option<u32>> {
    if workers < 2 || pids.is_empty() {
        let mut buf = String::with_capacity(512);
        return pids
            .iter()
            .map(|&p| d_threads_besides_main(root, p, &mut buf))
            .collect();
    }
    let mut out: Vec<Option<u32>> = vec![None; pids.len()];
    let chunk = pids.len().div_ceil(workers);
    // Cada fatia num slot: a thread que nasce tira a sua; a que o sistema
    // recusa (limite de pids, RLIMIT_NPROC) deixa a fatia para o chamador, e o
    // tick fica mais lento em vez de derrubar a thread do sampler.
    let slots: Vec<Mutex<Option<Slice<'_>>>> = pids
        .chunks(chunk)
        .zip(out.chunks_mut(chunk))
        .map(|part| Mutex::new(Some(part)))
        .collect();
    std::thread::scope(|s| {
        // A ultima fatia roda aqui mesmo: uma thread a menos para criar.
        let (last, rest) = slots.split_last().expect("pids is not empty");
        for slot in rest {
            if refuse_spawn()
                || std::thread::Builder::new()
                    .spawn_scoped(s, || run_slot(root, slot))
                    .is_err()
            {
                run_slot(root, slot);
            }
        }
        run_slot(root, last);
    });
    drop(slots);
    out
}

type Slice<'a> = (&'a [u32], &'a mut [Option<u32>]);

fn run_slot(root: &ProcRoot, slot: &Mutex<Option<Slice<'_>>>) {
    let part = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((ps, os)) = part {
        scan_slice(root, ps, os);
    }
}

#[cfg(not(test))]
fn refuse_spawn() -> bool {
    false
}

#[cfg(test)]
thread_local! {
    /// Faz o sistema "recusar" as threads de quem chama, nos testes.
    static REFUSE_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn refuse_spawn() -> bool {
    REFUSE_SPAWN.with(std::cell::Cell::get)
}

fn scan_slice(root: &ProcRoot, pids: &[u32], out: &mut [Option<u32>]) {
    let mut buf = String::with_capacity(512);
    for (&p, o) in pids.iter().zip(out.iter_mut()) {
        *o = d_threads_besides_main(root, p, &mut buf);
    }
}

#[cfg(test)]
mod dents_tests {
    use super::DentsBuf;

    /// `linux_dirent64` starts with a `u64`: the buffer the kernel fills has
    /// to be aligned for it, not just happen to be.
    #[test]
    fn getdents_buffer_is_aligned_for_linux_dirent64() {
        assert!(std::mem::align_of::<DentsBuf>() >= std::mem::align_of::<u64>());
        let b = DentsBuf::new();
        assert_eq!(b.as_bytes().as_ptr() as usize % std::mem::align_of::<u64>(), 0);
        assert_eq!(b.as_bytes().len(), super::DENTS_BUF);
    }
}

#[cfg(test)]
mod spawn_tests {
    use super::{REFUSE_SPAWN, scan_d_threads_with};
    use crate::fixture::{FakeProc, FakeProcess, TempDir};

    #[test]
    fn a_refused_thread_leaves_its_slice_to_the_caller() {
        let dir = TempDir::new("ac-refuse-spawn").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        let pids: Vec<u32> = (100..140).collect();
        for &pid in &pids {
            let mut p = FakeProcess::new(pid, 1, "worker");
            p.threads = 3;
            p.d_threads = 1;
            fp.add(&p).unwrap();
        }
        let root = fp.proc_root();
        let spawned = scan_d_threads_with(&root, &pids, 4);
        REFUSE_SPAWN.with(|r| r.set(true));
        let refused = scan_d_threads_with(&root, &pids, 4);
        REFUSE_SPAWN.with(|r| r.set(false));
        assert_eq!(refused, spawned);
        assert!(refused.iter().all(|n| n.is_some()), "{refused:?}");
    }
}
