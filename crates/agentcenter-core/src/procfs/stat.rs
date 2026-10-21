//! `/proc/<pid>/stat`.

use super::ReadError;
use super::statm::PAGE_SIZE;

/// Campos de `/proc/<pid>/stat` que o sampler usa. Tempos em ticks (USER_HZ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub utime: u64,
    pub stime: u64,
    pub num_threads: u32,
    /// Campo 22: instante de inicio em ticks desde o boot; distingue reuso de pid.
    pub starttime: u64,
    /// Campo 23: memoria virtual em bytes. Zero so sem `mm` (zumbi, thread de kernel).
    pub vsize: u64,
    /// Campo 24: RSS em paginas, o mesmo contador de `VmRSS` no status e do
    /// campo resident de `statm`.
    pub rss_pages: u64,
    /// Campo 42: ticks esperando E/S de bloco. Zero se `task_delayacct` desligado.
    pub delayacct_blkio_ticks: u64,
}

/// Faz o parse de uma linha de `stat`. O `comm` pode conter espaco e `)`, entao
/// ele vai do primeiro `(` ao ULTIMO `)`; os demais campos vem depois.
pub fn parse_stat(line: &str) -> Result<Stat, ReadError> {
    let open = line
        .find('(')
        .ok_or_else(|| ReadError::parse("stat: no '('"))?;
    let close = line
        .rfind(')')
        .ok_or_else(|| ReadError::parse("stat: no ')'"))?;
    if close < open {
        return Err(ReadError::parse("stat: ')' before '('"));
    }
    let pid = line[..open]
        .trim()
        .parse::<u32>()
        .map_err(|_| ReadError::parse("stat: pid"))?;
    let comm = line[open + 1..close].to_string();
    // rest[0] e o campo 3 (state); campo N fica em rest[N - 3].
    let rest: Vec<&str> = line[close + 1..].split_ascii_whitespace().collect();
    let field = |n: usize| -> Result<u64, ReadError> {
        rest.get(n - 3)
            .ok_or_else(|| ReadError::parse("stat: short line"))?
            .parse::<u64>()
            .map_err(|_| ReadError::parse("stat: number"))
    };
    let state = rest
        .first()
        .and_then(|s| s.chars().next())
        .ok_or_else(|| ReadError::parse("stat: state"))?;
    Ok(Stat {
        pid,
        comm,
        state,
        ppid: field(4)? as u32,
        utime: field(14)?,
        stime: field(15)?,
        num_threads: field(20)? as u32,
        starttime: field(22)?,
        vsize: field(23)?,
        rss_pages: field(24)?,
        // Kernels muito antigos nao tem o campo 42; trata como zero.
        delayacct_blkio_ticks: field(42).unwrap_or(0),
    })
}

impl Stat {
    /// RSS em bytes. `None` sem `mm` (zumbi, thread de kernel): o kernel escreve
    /// 0 em `vsize` e em `rss`, e o status nem tem a linha `VmRSS`. Processo com
    /// tudo em swap tem `vsize` e da `Some(0)`, que e uma medida.
    pub fn rss_bytes(&self) -> Option<u64> {
        (self.vsize != 0 || self.rss_pages != 0).then(|| self.rss_pages * PAGE_SIZE)
    }
}
