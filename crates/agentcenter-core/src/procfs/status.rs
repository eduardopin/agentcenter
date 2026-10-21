//! `/proc/<pid>/status`.

use super::ReadError;

/// O sampler le o status so na varredura completa (a cada 5 s), no processo
/// novo e na sessao selecionada; entre uma leitura e outra estes valores podem
/// estar ate 5 s defasados (ARCHITECTURE, "Sampling").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// Uid real.
    pub uid: u32,
    pub threads: u32,
    /// `VmRSS` em bytes: o mesmo contador que `stat` (campo 24) e `statm`
    /// (resident) expoem em paginas; o sampler usa o do `stat`, lido todo tick.
    /// `None` em thread de kernel, que nao tem a linha.
    pub rss_bytes: Option<u64>,
    /// `VmSwap` em bytes; `None` em thread de kernel. O sampler o usa, junto
    /// com o RSS do `stat`, para decidir quando reler `smaps_rollup`.
    pub swap_bytes: Option<u64>,
    pub voluntary_ctxt_switches: u64,
    pub nonvoluntary_ctxt_switches: u64,
}

pub fn parse_status(text: &str) -> Result<Status, ReadError> {
    let mut s = Status::default();
    let mut seen_uid = false;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let first = || {
            val.split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
        };
        match key {
            "Uid" => {
                s.uid = first().ok_or_else(|| ReadError::parse("status: Uid"))? as u32;
                seen_uid = true;
            }
            "Threads" => s.threads = first().unwrap_or(0) as u32,
            "VmRSS" => s.rss_bytes = super::kb_line_bytes(val),
            "VmSwap" => s.swap_bytes = super::kb_line_bytes(val),
            "voluntary_ctxt_switches" => s.voluntary_ctxt_switches = first().unwrap_or(0),
            "nonvoluntary_ctxt_switches" => s.nonvoluntary_ctxt_switches = first().unwrap_or(0),
            _ => {}
        }
    }
    if !seen_uid {
        return Err(ReadError::parse("status: no Uid"));
    }
    Ok(s)
}
