//! Arquivos do sistema: `/proc/stat` e `/proc/sys/kernel/task_delayacct`.

use super::{ProcRoot, ReadError};
use std::path::Path;

/// Linha `cpu` agregada de `/proc/stat`, em ticks, e numero de nucleos (`cpuN`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTotals {
    pub total: u64,
    /// idle + iowait.
    pub idle: u64,
    pub cores: u32,
    /// `btime`: boot time in seconds since the Unix epoch; 0 when absent.
    pub btime: u64,
}

/// Process start as ms since the Unix epoch: boot seconds (`btime`) plus the
/// `starttime` field of `stat` in USER_HZ ticks. One formula for every caller.
pub fn start_epoch_ms(btime_secs: u64, starttime_ticks: u64) -> u64 {
    btime_secs * 1000 + (starttime_ticks as f64 * 1000.0 / crate::sampler::USER_HZ) as u64
}

pub fn parse_proc_stat(text: &str) -> Result<CpuTotals, ReadError> {
    let mut out = CpuTotals::default();
    let mut seen = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let v: Vec<u64> = rest
                .split_ascii_whitespace()
                .map(|x| x.parse::<u64>().map_err(|_| ReadError::parse("stat: cpu")))
                .collect::<Result<_, _>>()?;
            // user nice system idle iowait irq softirq steal guest guest_nice;
            // guest e guest_nice ja estao contados em user e nice.
            out.total = v.iter().take(8).sum();
            out.idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
            seen = true;
        } else if line.starts_with("cpu") {
            out.cores += 1;
        } else if let Some(b) = line.strip_prefix("btime ") {
            out.btime = b.trim().parse().unwrap_or(0);
        }
    }
    if !seen {
        return Err(ReadError::parse("stat: no cpu line"));
    }
    out.cores = out.cores.max(1);
    Ok(out)
}

pub fn read_cpu_totals(root: &ProcRoot, buf: &mut String) -> Result<CpuTotals, ReadError> {
    root.read_into(Path::new("stat"), buf)?;
    parse_proc_stat(buf)
}

/// `true` se `kernel.task_delayacct` esta ligado. Arquivo ausente conta como desligado.
pub fn delayacct_enabled(root: &ProcRoot, buf: &mut String) -> bool {
    root.read_into(Path::new("sys/kernel/task_delayacct"), buf)
        .is_ok()
        && buf.trim() == "1"
}

#[cfg(test)]
mod tests {
    use super::start_epoch_ms;

    #[test]
    fn start_is_boot_seconds_plus_ticks_in_ms() {
        // 250 ticks at USER_HZ 100 are 2.5 s
        assert_eq!(start_epoch_ms(1_000, 250), 1_002_500);
        assert_eq!(start_epoch_ms(1_000, 0), 1_000_000);
    }
}
