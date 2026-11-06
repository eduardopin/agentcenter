//! Descritores abertos de `/proc/<pid>/stat` e `/proc/<pid>/io`, reaproveitados
//! de um tick para o outro.
//!
//! O tick le `stat` e `io` de cada processo de sessao todo segundo: ~750
//! leituras por tick, cada uma `openat` + `read` + `close`, e o `openat` e o
//! mais caro dos tres (resolve o caminho e monta o `seq_file`). Com o fd
//! guardado, a leitura vira um `pread` no deslocamento 0, que em `/proc`
//! regenera o conteudo.
//!
//! Corretude, em ordem do que o kernel garante para o que o cache confere:
//!
//! - Processo que acabou: o fd aponta para o `struct pid` antigo, e o `pread`
//!   devolve ESRCH (ou 0 bytes). Erro ou leitura vazia derrubam o fd e a
//!   leitura e refeita UMA vez pelo caminho, que da o mesmo `Gone` de antes.
//! - Pid reusado: o processo novo tem outro `struct pid`, entao o fd antigo
//!   nunca le o novo. Mesmo assim cada entrada guarda o `starttime` do processo
//!   para o qual foi aberta: o `stat` lido tem de trazer o mesmo pid e o mesmo
//!   `starttime`, e o `io` so vale se o chamador (que ja leu o `stat`) pede com
//!   o mesmo `starttime`. Divergiu: o fd cai e a leitura vai pelo caminho.
//! - Raiz que nao e procfs (fixture): um arquivo apagado e recriado continua
//!   legivel pelo fd antigo, e nada no conteudo denuncia isso. Por isso, no
//!   tick de varredura completa (`refresh`) toda entrada e reaberta pelo
//!   caminho: o descompasso dura no maximo ate a proxima varredura, e em
//!   `/proc` real nao existe.
//!
//! Custo limitado: o orcamento padrao e o limite de fds do processo
//! (`RLIMIT_NOFILE`, lido de `/proc/self/limits`) menos uma folga (ver
//! [`default_budget`]), no maximo dois fds por pid vivo do tick, e as entradas
//! de quem saiu da sessao fecham na hora. Alem disso, ou em EMFILE/ENFILE, a
//! leitura segue o caminho `open`-`read`-`close` de sempre. Os fds sao do [`FdCache`], que
//! e do `Sampler`: soltar o amostrador fecha todos.
//!
//! Sem `unsafe`: o `pread` e o `FileExt::read_at` da std.

use super::io::{IoCounters, parse_io};
use super::stat::{Stat, parse_stat};
use super::{ProcRoot, ReadError, pread_into, read_open_into};
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

/// Quantas raizes distintas o cache segura ao mesmo tempo (o benchmark do tick
/// alterna duas; o sampler real usa uma). A menos usada cai para dar lugar.
const MAX_ROOTS: usize = 4;

/// Teto absoluto de fds guardados, qualquer que seja o limite do processo.
const MAX_BUDGET: usize = 8192;

/// Orcamento quando o limite nao pode ser lido (sem `/proc/self/limits`).
const FALLBACK_BUDGET: usize = 256;

/// Folga minima que o orcamento padrao deixa livre no limite de fds para o
/// resto do processo (sockets, arquivos de sessao, GTK, as aberturas
/// transitorias do proprio tick).
const MIN_SLACK: u64 = 256;

/// EMFILE e ENFILE: sem fd livre, no processo ou no sistema.
const EMFILE: i32 = 24;
const ENFILE: i32 = 23;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Stat,
    Io,
}

struct Entry {
    file: File,
    /// `starttime` do processo para o qual o fd foi aberto.
    start: u64,
}

/// Entradas de uma raiz.
struct Table {
    path: PathBuf,
    last_used: u64,
    map: HashMap<(u32, Kind), Entry>,
}

/// Contadores para teste e diagnostico.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FdCacheStats {
    /// Leituras atendidas pelo fd guardado (1 syscall).
    pub hits: u64,
    /// Fd guardado que foi recusado (erro, vazio, pid ou `starttime` outro) e
    /// reaberto pelo caminho na mesma leitura.
    pub rejected: u64,
    /// Fds guardados depois de uma abertura pelo caminho.
    pub kept: u64,
    /// Aberturas que nao puderam guardar o fd por falta de orcamento.
    pub over_budget: u64,
}

pub struct FdCache {
    tables: Vec<Table>,
    active: usize,
    /// Teto configurado (do limite do processo); 0 desliga o cache.
    budget: usize,
    /// Teto por raiz neste tick: dois fds (`stat` e `io`) por pid vivo.
    per_root: usize,
    /// Fds guardados, somados todas as raizes.
    held: usize,
    clock: u64,
    stats: FdCacheStats,
}

impl Default for FdCache {
    fn default() -> Self {
        Self::with_budget(default_budget())
    }
}

impl FdCache {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            tables: Vec::new(),
            active: 0,
            budget,
            per_root: usize::MAX,
            held: 0,
            clock: 0,
            stats: FdCacheStats::default(),
        }
    }

    /// Troca o teto de fds guardados; 0 desliga o cache e fecha tudo.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.evict_to(budget);
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Fds guardados agora.
    pub fn held(&self) -> usize {
        self.held
    }

    pub fn stats(&self) -> FdCacheStats {
        self.stats
    }

    /// Abre um tick com `live` pids vivos listados: uma raiz nao guarda mais de
    /// dois fds (`stat` e `io`) por pid vivo.
    pub fn begin_tick(&mut self, live: usize) {
        self.clock += 1;
        self.per_root = live.saturating_mul(2);
    }

    /// Solta as entradas da raiz ativa cujo pid `live` nega (processo que saiu
    /// da sessao ou da listagem): o fd fecha na hora.
    pub fn retain_pids(&mut self, mut live: impl FnMut(u32) -> bool) {
        if let Some(t) = self.tables.get_mut(self.active) {
            let before = t.map.len();
            t.map.retain(|(pid, _), _| live(*pid));
            self.held -= before - t.map.len();
        }
    }

    /// `stat` do `pid` lido para `buf` e interpretado. `keep`: guardar o fd
    /// (o pid e lido todo tick). `refresh`: ignorar o fd guardado e reabrir.
    pub fn read_stat(
        &mut self,
        root: &ProcRoot,
        pid: u32,
        keep: bool,
        refresh: bool,
        buf: &mut String,
    ) -> Result<Stat, ReadError> {
        root.note_read(pid, "stat");
        let ti = self.table(root);
        let key = (pid, Kind::Stat);
        if refresh {
            self.drop_entry(ti, key);
        } else if let Some(e) = self.tables[ti].map.get(&key) {
            if pread_into(&e.file, buf).is_ok()
                && !buf.is_empty()
                && let Ok(st) = parse_stat(buf)
                && st.pid == pid
                && st.starttime == e.start
            {
                self.stats.hits += 1;
                return Ok(st);
            }
            self.stats.rejected += 1;
            self.drop_entry(ti, key);
        }
        let mut f = self.open(root, pid, "stat")?;
        read_open_into(&mut f, buf)?;
        let st = parse_stat(buf)?;
        if keep {
            self.keep(ti, key, f, st.starttime);
        }
        Ok(st)
    }

    /// `io` do `pid`, ou `None` se ilegivel, ausente ou malformado (como
    /// [`super::io::read_io`]). `start` e o `starttime` do processo, lido do
    /// `stat` deste tick: fd aberto para outro `starttime` nao vale.
    pub fn read_io(
        &mut self,
        root: &ProcRoot,
        pid: u32,
        start: u64,
        keep: bool,
        refresh: bool,
        buf: &mut String,
    ) -> Option<IoCounters> {
        root.note_read(pid, "io");
        let ti = self.table(root);
        let key = (pid, Kind::Io);
        if refresh {
            self.drop_entry(ti, key);
        } else if let Some(e) = self.tables[ti].map.get(&key) {
            if e.start == start
                && pread_into(&e.file, buf).is_ok()
                && !buf.is_empty()
                && let Ok(io) = parse_io(buf)
            {
                self.stats.hits += 1;
                return Some(io);
            }
            self.stats.rejected += 1;
            self.drop_entry(ti, key);
        }
        let mut f = self.open(root, pid, "io").ok()?;
        read_open_into(&mut f, buf).ok()?;
        let io = parse_io(buf).ok()?;
        if keep {
            self.keep(ti, key, f, start);
        }
        Some(io)
    }

    /// Indice da tabela da raiz (criada se nova; a menos usada cai no teto).
    fn table(&mut self, root: &ProcRoot) -> usize {
        let path = root.path().as_os_str();
        if let Some(t) = self.tables.get(self.active)
            && t.path.as_os_str() == path
        {
            return self.active;
        }
        let found = self.tables.iter().position(|t| t.path.as_os_str() == path);
        let ti = match found {
            Some(i) => i,
            None => {
                if self.tables.len() >= MAX_ROOTS {
                    let lru = (0..self.tables.len())
                        .min_by_key(|&i| self.tables[i].last_used)
                        .unwrap_or(0);
                    self.held -= self.tables.swap_remove(lru).map.len();
                }
                self.tables.push(Table {
                    path: root.path().to_path_buf(),
                    last_used: 0,
                    map: HashMap::new(),
                });
                self.tables.len() - 1
            }
        };
        self.tables[ti].last_used = self.clock;
        self.active = ti;
        ti
    }

    fn drop_entry(&mut self, ti: usize, key: (u32, Kind)) {
        if self.tables[ti].map.remove(&key).is_some() {
            self.held -= 1;
        }
    }

    /// Guarda `file` se couber no orcamento deste tick; senao ele fecha aqui.
    fn keep(&mut self, ti: usize, key: (u32, Kind), file: File, start: u64) {
        if self.held >= self.budget || self.tables[ti].map.len() >= self.per_root {
            self.stats.over_budget += 1;
            return;
        }
        if self.tables[ti]
            .map
            .insert(key, Entry { file, start })
            .is_none()
        {
            self.held += 1;
        }
        self.stats.kept += 1;
    }

    /// Abre `<raiz>/<pid>/<file>`. Sem fd livre (EMFILE/ENFILE) o cache e o
    /// primeiro suspeito: devolve metade dos fds, reduz o orcamento ao que sobrou
    /// e tenta de novo uma vez.
    fn open(&mut self, root: &ProcRoot, pid: u32, file: &str) -> Result<File, ReadError> {
        let open = |root: &ProcRoot| root.with_pid_path(pid, file, |p| File::open(p));
        match open(root) {
            Err(e) if matches!(e.raw_os_error(), Some(EMFILE | ENFILE)) && self.held > 0 => {
                self.budget = self.held / 2;
                self.evict_to(self.budget);
                open(root).map_err(ReadError::from_io)
            }
            r => r.map_err(ReadError::from_io),
        }
    }

    /// Fecha entradas (das raizes menos usadas primeiro) ate sobrarem `n`.
    fn evict_to(&mut self, n: usize) {
        if self.held <= n {
            return;
        }
        let mut order: Vec<usize> = (0..self.tables.len()).collect();
        order.sort_by_key(|&i| (i == self.active, self.tables[i].last_used));
        for i in order {
            let map = &mut self.tables[i].map;
            let over = self.held - n;
            let keys: Vec<_> = map.keys().take(over).copied().collect();
            for k in keys {
                map.remove(&k);
                self.held -= 1;
            }
            if self.held <= n {
                break;
            }
        }
    }
}

/// Orcamento padrao: o limite brando de fds do processo menos uma folga de
/// `max(256, limite / 4)`, ate [`MAX_BUDGET`]. Metade do limite (o primeiro
/// desenho) dava 512 com o `ulimit -n 1024` padrao do systemd, abaixo dos ~748
/// fds de uma raiz com 374 processos de sessao, e o ganho sumia justo onde o
/// limite e mais apertado; com a folga fixa de 256 (um quarto em limites
/// altos) o 1024 vira 768, o bastante para uma raiz, e o resto do processo
/// ainda tem onde abrir. Limite ilegivel: [`FALLBACK_BUDGET`]; `unlimited`:
/// [`MAX_BUDGET`]; limite menor que a folga: 0 (cache desligado).
fn default_budget() -> usize {
    budget_for_limit(
        std::fs::read_to_string("/proc/self/limits")
            .ok()
            .as_deref()
            .and_then(nofile_soft_limit),
    )
}

fn budget_for_limit(soft: Option<u64>) -> usize {
    soft.map_or(FALLBACK_BUDGET, |soft| {
        let slack = MIN_SLACK.max(soft / 4);
        soft.saturating_sub(slack).min(MAX_BUDGET as u64) as usize
    })
}

/// Limite brando de fds (`Max open files`) de um `/proc/<pid>/limits`.
/// `unlimited` vira `u64::MAX`.
pub(crate) fn nofile_soft_limit(limits: &str) -> Option<u64> {
    let rest = limits
        .lines()
        .find_map(|l| l.strip_prefix("Max open files"))?;
    match rest.split_whitespace().next()? {
        "unlimited" => Some(u64::MAX),
        n => n.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FakeProc, FakeProcess, TempDir};
    use std::fs;
    use std::path::Path;

    fn proc_with(dir: &TempDir, pids: &[u32]) -> (FakeProc, ProcRoot) {
        let fp = FakeProc::create(dir.path()).unwrap();
        for &pid in pids {
            fp.add(&FakeProcess::new(pid, 1, "bash")).unwrap();
        }
        let root = fp.proc_root();
        (fp, root)
    }

    fn io_text(read: u64) -> String {
        format!(
            "rchar: 1\nwchar: 2\nsyscr: 0\nsyscw: 0\nread_bytes: {read}\n\
             write_bytes: 0\ncancelled_write_bytes: 0\n"
        )
    }

    /// Troca `path` por um arquivo novo (outro inode), como um processo novo
    /// no mesmo pid; o inode antigo segue legivel pelo fd guardado.
    fn replace_file(path: &Path, text: &str) {
        let tmp = path.with_extension("novo");
        fs::write(&tmp, text).unwrap();
        fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn nofile_soft_limit_reads_the_soft_column() {
        let text = "Limit                     Soft Limit           Hard Limit           Units\n\
                    Max cpu time              unlimited            unlimited            seconds\n\
                    Max open files            1024                 524288               files\n\
                    Max locked memory         8388608              8388608              bytes\n";
        assert_eq!(nofile_soft_limit(text), Some(1024));
        let unl = "Max open files            unlimited            unlimited            files\n";
        assert_eq!(nofile_soft_limit(unl), Some(u64::MAX));
        assert_eq!(
            nofile_soft_limit("Max cpu time unlimited unlimited\n"),
            None
        );
        assert_eq!(nofile_soft_limit("Max open files   lixo  1  files\n"), None);
    }

    #[test]
    fn default_budget_leaves_room_below_the_soft_limit() {
        // O padrao do systemd (1024) tem de cobrir uma raiz de 374 processos de
        // sessao (748 fds); metade do limite dava 512.
        assert_eq!(budget_for_limit(Some(1024)), 768);
        assert!(budget_for_limit(Some(1024)) >= 2 * 374);
        // Folga de um quarto em limites altos, de 256 nos baixos.
        assert_eq!(budget_for_limit(Some(4096)), 3072);
        assert_eq!(budget_for_limit(Some(512)), 256);
        assert_eq!(budget_for_limit(Some(256)), 0);
        assert_eq!(budget_for_limit(Some(100)), 0);
        // Teto absoluto, `unlimited` e limite ilegivel.
        assert_eq!(budget_for_limit(Some(524_288)), MAX_BUDGET);
        assert_eq!(budget_for_limit(Some(u64::MAX)), MAX_BUDGET);
        assert_eq!(budget_for_limit(None), FALLBACK_BUDGET);
        // E o desta maquina respeita a mesma conta.
        let limits = fs::read_to_string("/proc/self/limits").unwrap();
        let soft = nofile_soft_limit(&limits).unwrap();
        assert_eq!(FdCache::default().budget(), budget_for_limit(Some(soft)));
    }

    #[test]
    fn cached_stat_rereads_fresh_content_each_time() {
        let dir = TempDir::new("ac-fd-stat").unwrap();
        let (fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(1);
        let first = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        assert_eq!(c.held(), 1);
        for utime in [5, 600, 70_000] {
            let mut p = FakeProcess::new(7, 1, "bash");
            p.utime = utime;
            fp.add(&p).unwrap();
            c.begin_tick(1);
            let st = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
            let want = crate::procfs::stat::parse_stat(
                &fs::read_to_string(root.pid_path(7, "stat")).unwrap(),
            )
            .unwrap();
            assert_eq!(st, want);
            assert_eq!(st.utime, utime);
        }
        assert_eq!(first.utime, 0);
        assert_eq!(c.stats().hits, 3, "as tres leituras vieram do fd guardado");
        assert_eq!(c.stats().rejected, 0);
    }

    #[test]
    fn reused_pid_with_other_starttime_drops_the_stat_fd() {
        let dir = TempDir::new("ac-fd-reuse").unwrap();
        let (fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(1);
        let old = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        // Outro processo no pid 7: mesmo arquivo, outro starttime e comm.
        let mut p = FakeProcess::new(7, 1, "claude");
        p.starttime = 99_999;
        fp.add(&p).unwrap();
        c.begin_tick(1);
        let new = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        assert_eq!((old.starttime, old.comm.as_str()), (1000, "bash"));
        assert_eq!((new.starttime, new.comm.as_str()), (99_999, "claude"));
        assert_eq!(c.stats().rejected, 1, "o starttime diferente derrubou o fd");
        assert_eq!(c.stats().hits, 0);
        // O fd reaberto ja serve ao processo novo.
        c.begin_tick(1);
        assert_eq!(
            c.read_stat(&root, 7, true, false, &mut buf)
                .unwrap()
                .starttime,
            99_999
        );
        assert_eq!(c.stats().hits, 1);
    }

    #[test]
    fn stat_of_another_pid_in_the_file_is_not_served_from_the_fd() {
        let dir = TempDir::new("ac-fd-otherpid").unwrap();
        let (fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(1);
        c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        // O conteudo agora e de outro pid (stat de outro processo no mesmo arquivo).
        fs::write(
            root.pid_path(7, "stat"),
            FakeProcess::new(8, 1, "node").stat_line(),
        )
        .unwrap();
        drop(fp);
        c.begin_tick(1);
        let st = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        assert_eq!(st.pid, 8, "o caminho e a verdade: mesma resposta de antes");
        assert_eq!(c.stats().rejected, 1);
    }

    #[test]
    fn io_fd_opened_for_another_starttime_is_not_used() {
        let dir = TempDir::new("ac-fd-io-reuse").unwrap();
        let (_fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        let path = root.pid_path(7, "io");
        fs::write(&path, io_text(111)).unwrap();
        c.begin_tick(1);
        let old = c.read_io(&root, 7, 1000, true, false, &mut buf).unwrap();
        assert_eq!(old.read_bytes, 111);
        // Pid reusado: o processo novo tem outro `io` (outro inode). O fd antigo
        // ainda le 111; so o starttime impede que ele seja atribuido ao novo.
        replace_file(&path, &io_text(222));
        c.begin_tick(1);
        let new = c.read_io(&root, 7, 2000, true, false, &mut buf).unwrap();
        assert_eq!(new.read_bytes, 222, "io do processo novo, nao do antigo");
        assert_eq!(c.stats().rejected, 1);
        assert_eq!(c.held(), 1);
        // E o fd novo, do starttime novo, passa a servir.
        c.begin_tick(1);
        let again = c.read_io(&root, 7, 2000, true, false, &mut buf).unwrap();
        assert_eq!(again.read_bytes, 222);
        assert_eq!(c.stats().hits, 1);
    }

    #[test]
    fn empty_read_from_the_cached_fd_reopens_by_path_once() {
        let dir = TempDir::new("ac-fd-empty").unwrap();
        let (_fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        let path = root.pid_path(7, "io");
        fs::write(&path, io_text(5)).unwrap();
        c.begin_tick(1);
        c.read_io(&root, 7, 1000, true, false, &mut buf).unwrap();
        // O inode que o fd guarda esvazia (como o `/proc` de quem morreu: 0
        // bytes) e o caminho passa a ter conteudo valido.
        fs::write(&path, "").unwrap();
        replace_file(&path, &io_text(9));
        c.begin_tick(1);
        let io = c.read_io(&root, 7, 1000, true, false, &mut buf).unwrap();
        assert_eq!(io.read_bytes, 9);
        assert_eq!(c.stats().rejected, 1);
        assert_eq!(c.stats().hits, 0);
        assert_eq!(c.held(), 1, "o fd novo ocupa o lugar do antigo");
    }

    #[test]
    fn unreadable_io_is_none_and_holds_no_fd() {
        let dir = TempDir::new("ac-fd-io-denied").unwrap();
        let (_fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        let path = root.pid_path(7, "io");
        // Ausente.
        fs::remove_file(&path).unwrap();
        c.begin_tick(1);
        assert!(c.read_io(&root, 7, 1000, true, false, &mut buf).is_none());
        // Ilegivel (um diretorio abre e falha no read, como o EACCES do kernel
        // que aparece na leitura) e malformado.
        fs::create_dir(&path).unwrap();
        assert!(c.read_io(&root, 7, 1000, true, false, &mut buf).is_none());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, "rchar: x\n").unwrap();
        assert!(c.read_io(&root, 7, 1000, true, false, &mut buf).is_none());
        assert_eq!((c.held(), c.stats().kept), (0, 0));
    }

    #[test]
    fn vanished_process_on_real_proc_is_gone_and_its_fd_is_dropped() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let root = ProcRoot::default();
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(1);
        let st = c.read_stat(&root, pid, true, false, &mut buf).unwrap();
        assert_eq!(st.comm, "sleep");
        c.begin_tick(1);
        c.read_stat(&root, pid, true, false, &mut buf).unwrap();
        assert_eq!((c.held(), c.stats().hits), (1, 1));
        child.kill().unwrap();
        child.wait().unwrap();
        // Processo reaped: pread no fd antigo da ESRCH, o caminho da ENOENT.
        c.begin_tick(1);
        assert_eq!(
            c.read_stat(&root, pid, true, false, &mut buf),
            Err(ReadError::Gone)
        );
        assert_eq!(c.held(), 0);
        assert_eq!(c.stats().rejected, 1);
    }

    #[test]
    fn budget_and_per_root_bound_cap_the_fds_and_reads_still_work() {
        let dir = TempDir::new("ac-fd-budget").unwrap();
        let pids: Vec<u32> = (10..30).collect();
        let (_fp, root) = proc_with(&dir, &pids);
        let mut buf = String::new();
        let mut want = FdCache::with_budget(0);
        for (budget, live, cap) in [(5, 20, 5), (100, 3, 6), (0, 20, 0), (100, 20, 20)] {
            let mut c = FdCache::with_budget(budget);
            c.begin_tick(live);
            for _ in 0..2 {
                for &pid in &pids {
                    let a = c.read_stat(&root, pid, true, false, &mut buf).unwrap();
                    assert_eq!(
                        a,
                        want.read_stat(&root, pid, true, false, &mut buf).unwrap()
                    );
                }
                assert_eq!(c.held(), cap, "budget {budget} live {live}");
            }
        }
        // Reduzir o orcamento fecha o excesso na hora.
        let mut c = FdCache::with_budget(100);
        c.begin_tick(20);
        for &pid in &pids {
            c.read_stat(&root, pid, true, false, &mut buf).unwrap();
        }
        assert_eq!(c.held(), 20);
        c.set_budget(4);
        assert_eq!(c.held(), 4);
        c.set_budget(0);
        assert_eq!(c.held(), 0);
    }

    #[test]
    fn refresh_reopens_every_entry_by_path() {
        let dir = TempDir::new("ac-fd-refresh").unwrap();
        let (_fp, root) = proc_with(&dir, &[7]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(1);
        c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        // Processo "recriado" (arquivo novo, mesmo conteudo de identidade): o fd
        // antigo nao denuncia; a releitura da varredura completa troca o fd.
        let mut p = FakeProcess::new(7, 1, "bash");
        p.utime = 42;
        replace_file(&root.pid_path(7, "stat"), &p.stat_line());
        c.begin_tick(1);
        let stale = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        assert_eq!(stale.utime, 0, "fd guardado ainda le o inode antigo");
        let fresh = c.read_stat(&root, 7, true, true, &mut buf).unwrap();
        assert_eq!(fresh.utime, 42, "refresh le pelo caminho");
        assert_eq!(c.held(), 1);
        let after = c.read_stat(&root, 7, true, false, &mut buf).unwrap();
        assert_eq!(after.utime, 42);
    }

    #[test]
    fn roots_do_not_mix_and_the_least_used_one_gives_way() {
        let dirs: Vec<TempDir> = (0..MAX_ROOTS + 1)
            .map(|i| TempDir::new(&format!("ac-fd-root{i}")).unwrap())
            .collect();
        let roots: Vec<ProcRoot> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let fp = FakeProc::create(d.path()).unwrap();
                let mut p = FakeProcess::new(7, 1, "bash");
                p.utime = i as u64;
                fp.add(&p).unwrap();
                fp.proc_root()
            })
            .collect();
        let mut c = FdCache::default();
        let mut buf = String::new();
        for round in 0..3 {
            for (i, r) in roots.iter().enumerate() {
                c.begin_tick(1);
                let st = c.read_stat(r, 7, true, false, &mut buf).unwrap();
                assert_eq!(st.utime, i as u64, "round {round}");
            }
        }
        assert!(c.held() <= MAX_ROOTS, "uma raiz saiu para dar lugar");
        assert_eq!(c.tables.len(), MAX_ROOTS);
    }

    #[test]
    fn retain_pids_closes_the_fds_of_the_rest() {
        let dir = TempDir::new("ac-fd-retain").unwrap();
        let (_fp, root) = proc_with(&dir, &[7, 8, 9]);
        let mut c = FdCache::default();
        let mut buf = String::new();
        c.begin_tick(3);
        for pid in [7, 8, 9] {
            c.read_stat(&root, pid, true, false, &mut buf).unwrap();
            c.read_io(&root, pid, 1000, true, false, &mut buf).unwrap();
        }
        assert_eq!(c.held(), 6);
        c.retain_pids(|pid| pid == 8);
        assert_eq!(c.held(), 2);
        assert_eq!(c.tables[0].map.len(), 2);
    }

    /// Sem fd livre: o cache devolve metade, reduz o orcamento e a leitura que
    /// falhou passa. Roda num filho com `ulimit -n` baixo (o limite e do processo
    /// inteiro, e os outros testes rodam em threads do mesmo processo).
    #[test]
    fn out_of_fds_sheds_the_cache_and_the_read_still_succeeds() {
        const CHILD: &str = "AC_FDCACHE_EMFILE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let exe = std::env::current_exe().unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(
                    "ulimit -n 48 && exec \"$0\" --exact \
                     procfs::fdcache::tests::out_of_fds_sheds_the_cache_and_the_read_still_succeeds \
                     --test-threads=1 --nocapture",
                )
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
        let dir = TempDir::new("ac-fd-emfile").unwrap();
        let pids: Vec<u32> = (10..110).collect();
        let (_fp, root) = proc_with(&dir, &pids);
        let mut c = FdCache::with_budget(10_000);
        let mut buf = String::new();
        c.begin_tick(pids.len());
        for &pid in &pids {
            let st = c.read_stat(&root, pid, true, false, &mut buf);
            assert_eq!(st.unwrap().pid, pid, "leitura {pid} tem de passar");
        }
        // Coube bem menos que os 100 pedidos; o orcamento acompanha o que sobrou.
        assert!(c.held() > 0 && c.held() < 48, "held {}", c.held());
        assert!(c.budget() < 48, "budget {}", c.budget());
    }
}
