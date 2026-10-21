//! Leitor proprio de `/proc` (ADR-001 #4): le so o que o modelo de sessao usa,
//! a partir de uma raiz configuravel para que os testes usem arvores falsas.

pub mod io;
pub mod smaps;
pub mod stat;
pub mod statm;
pub mod status;
pub mod system;
pub mod task;

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Leituras contadas por (pid, arquivo).
type ReadLog = Arc<Mutex<HashMap<(u32, &'static str), u32>>>;

/// Raiz do procfs. `ProcRoot::default()` e `/proc`; os testes apontam para uma fixture.
#[derive(Debug, Clone)]
pub struct ProcRoot {
    path: PathBuf,
    /// So em teste ([`ProcRoot::counting`]): leituras caras por (pid, arquivo).
    reads: Option<ReadLog>,
}

/// Duas raizes sao iguais se apontam para o mesmo lugar; o contador nao conta.
impl PartialEq for ProcRoot {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for ProcRoot {}

impl Default for ProcRoot {
    fn default() -> Self {
        Self::new("/proc")
    }
}

impl ProcRoot {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            reads: None,
        }
    }

    /// Raiz que conta as leituras caras feitas por [`ProcRoot::read_pid`]
    /// (`smaps_rollup`, varredura de `task/`, `stat`, `io`, `status`, `cmdline`)
    /// e por [`ProcRoot::read_link_pid`] (`cwd`). Para testar a cadencia do sampler.
    pub fn counting(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            reads: Some(Arc::default()),
        }
    }

    /// Quantas vezes `file` do `pid` foi lido. 0 numa raiz sem contador.
    pub fn read_count(&self, pid: u32, file: &str) -> u32 {
        let Some(reads) = &self.reads else { return 0 };
        let reads = reads.lock().unwrap_or_else(|e| e.into_inner());
        reads
            .iter()
            .filter(|((p, f), _)| *p == pid && *f == file)
            .map(|(_, n)| *n)
            .sum()
    }

    /// Registra uma leitura cara (so numa raiz [`ProcRoot::counting`]).
    pub(crate) fn note_read(&self, pid: u32, file: &'static str) {
        if let Some(reads) = &self.reads {
            *reads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry((pid, file))
                .or_default() += 1;
        }
    }

    /// Le `<raiz>/<pid>/<file>` para `buf`, contando a leitura.
    pub fn read_pid(
        &self,
        pid: u32,
        file: &'static str,
        buf: &mut String,
    ) -> Result<(), ReadError> {
        self.note_read(pid, file);
        read_file_into(&self.pid_path(pid, file), buf)
    }

    /// Destino do link `<raiz>/<pid>/<file>` (`cwd`), contando a leitura.
    pub fn read_link_pid(&self, pid: u32, file: &'static str) -> Option<PathBuf> {
        self.note_read(pid, file);
        std::fs::read_link(self.pid_path(pid, file)).ok()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pid_path(&self, pid: u32, file: &str) -> PathBuf {
        let mut p = self.path.join(pid.to_string());
        if !file.is_empty() {
            p.push(file);
        }
        p
    }

    /// Lista os diretorios numericos da raiz (processos). Ordem nao garantida.
    pub fn pids(&self) -> Result<Vec<u32>, ReadError> {
        let mut out = Vec::with_capacity(1024);
        let dir = std::fs::read_dir(&self.path).map_err(ReadError::from_io)?;
        for entry in dir.flatten() {
            if let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            {
                out.push(pid);
            }
        }
        Ok(out)
    }

    /// Le `<raiz>/<rel>` para `buf` (limpo antes), reaproveitando a alocacao.
    pub fn read_into(&self, rel: &Path, buf: &mut String) -> Result<(), ReadError> {
        read_file_into(&self.path.join(rel), buf)
    }
}

/// Bloco de leitura. Os arquivos de texto lidos por tick (stat, status, statm,
/// smaps_rollup, io) cabem num bloco; os maiores pedem mais de uma volta.
const CHUNK: usize = 8192;

/// Le o arquivo inteiro para `buf`. Em `/proc` (seq_file) e em tmpfs uma leitura
/// curta so acontece no fim do arquivo, entao paramos nela em vez de gastar mais
/// um `read` para ver o 0; e nao usamos `read_to_string`, que faz statx + lseek
/// para estimar o tamanho (sempre 0 em `/proc`). De 6 syscalls por arquivo para 3.
pub(crate) fn read_file_into(path: &Path, buf: &mut String) -> Result<(), ReadError> {
    buf.clear();
    let mut f = File::open(path).map_err(ReadError::from_io)?;
    let mut bytes = std::mem::take(buf).into_bytes();
    let res = loop {
        let len = bytes.len();
        bytes.resize(len + CHUNK, 0);
        match f.read(&mut bytes[len..]) {
            Ok(n) => {
                bytes.truncate(len + n);
                if n < CHUNK {
                    break Ok(());
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => bytes.truncate(len),
            Err(e) => {
                bytes.truncate(len);
                break Err(ReadError::from_io(e));
            }
        }
    };
    match String::from_utf8(bytes) {
        Ok(s) => *buf = s,
        Err(e) => {
            // comm pode ter bytes nao UTF-8; o resto do arquivo continua util.
            *buf = String::from_utf8_lossy(e.as_bytes()).into_owned();
        }
    }
    res
}

/// Erro de leitura classificado pelo que o chamador faz com ele.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// O processo (ou o arquivo) sumiu entre a listagem e a leitura: descartar.
    Gone,
    /// Sem permissao (processo de outro usuario): campo vira `None`.
    Denied,
    /// Conteudo fora do formato esperado.
    Parse(String),
    /// Qualquer outro erro de E/S.
    Io(String),
}

impl ReadError {
    pub(crate) fn from_io(e: std::io::Error) -> Self {
        // ESRCH (3): o kernel devolve isso ao ler arquivos de um processo que acabou de morrer.
        if e.kind() == ErrorKind::NotFound || e.raw_os_error() == Some(3) {
            ReadError::Gone
        } else if e.kind() == ErrorKind::PermissionDenied {
            ReadError::Denied
        } else {
            ReadError::Io(e.to_string())
        }
    }

    pub(crate) fn parse(what: &str) -> Self {
        ReadError::Parse(what.to_string())
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Gone => f.write_str("process gone"),
            ReadError::Denied => f.write_str("permission denied"),
            ReadError::Parse(w) => write!(f, "parse error: {w}"),
            ReadError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Valor em kB de uma linha `Chave:   123 kB`, convertido para bytes.
pub(crate) fn kb_line_bytes(rest: &str) -> Option<u64> {
    let n = rest.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(n * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn from_io_classifies_gone_denied_and_other() {
        let raw = std::io::Error::from_raw_os_error;
        assert_eq!(ReadError::from_io(raw(3)), ReadError::Gone, "ESRCH");
        assert_eq!(ReadError::from_io(raw(2)), ReadError::Gone, "ENOENT");
        assert_eq!(ReadError::from_io(raw(13)), ReadError::Denied, "EACCES");
        assert!(
            matches!(ReadError::from_io(raw(21)), ReadError::Io(_)),
            "EISDIR"
        );
    }

    #[test]
    fn read_file_into_stops_at_first_short_read() {
        // FIFO: o escritor so manda o resto depois que a leitura volta. Quem pedir
        // mais um read depois de um bloco cheio (bloco menor que o arquivo, ou sem
        // parar na leitura curta) espera esse resto e o devolve junto.
        let dir = crate::fixture::TempDir::new("ac-fifo").unwrap();
        let fifo = dir.path().join("f");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let path = fifo.clone();
        let writer = std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            // 4096 bytes = PIPE_BUF: chega inteiro num read so.
            f.write_all(&[b'a'; 4096]).unwrap();
            let _ = done_rx.recv_timeout(Duration::from_secs(2));
            let _ = f.write_all(b"TAIL");
        });
        let mut buf = String::new();
        read_file_into(&fifo, &mut buf).unwrap();
        let _ = done_tx.send(());
        writer.join().unwrap();
        assert_eq!(buf.len(), 4096, "leu alem da leitura curta");
    }
}
