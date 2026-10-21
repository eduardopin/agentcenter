//! `/proc/<pid>/io`. So o dono (ou root) le; o resto vira `None`.

use super::ReadError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IoCounters {
    /// Bytes que chegaram ao armazenamento (trafego de disco).
    pub read_bytes: u64,
    pub write_bytes: u64,
    /// Bytes de syscalls read/write ("syscall I/O", inclui cache e pipes).
    pub rchar: u64,
    pub wchar: u64,
}

pub fn parse_io(text: &str) -> Result<IoCounters, ReadError> {
    let mut c = IoCounters::default();
    let mut seen = 0;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let v = || {
            val.trim()
                .parse::<u64>()
                .map_err(|_| ReadError::parse("io: number"))
        };
        match key {
            "read_bytes" => c.read_bytes = v()?,
            "write_bytes" => c.write_bytes = v()?,
            "rchar" => c.rchar = v()?,
            "wchar" => c.wchar = v()?,
            _ => continue,
        }
        seen += 1;
    }
    if seen < 4 {
        return Err(ReadError::parse("io: missing fields"));
    }
    Ok(c)
}

/// Le `/proc/<pid>/io`. Ilegivel (EACCES de outro usuario), ausente ou malformado
/// vira `None`: o processo continua medido, so sem disco. Contada numa raiz
/// [`super::ProcRoot::counting`], como `stat` e `smaps_rollup`.
pub fn read_io(root: &super::ProcRoot, pid: u32, buf: &mut String) -> Option<IoCounters> {
    root.read_pid(pid, "io", buf).ok()?;
    parse_io(buf).ok()
}
