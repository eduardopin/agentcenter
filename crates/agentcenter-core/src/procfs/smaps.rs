//! `/proc/<pid>/smaps_rollup`: memoria proporcional (PSS) e swap.

use super::{ReadError, kb_line_bytes};

/// Valores em bytes (o arquivo traz kB).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmapsRollup {
    pub pss: u64,
    pub swap: u64,
    pub swap_pss: u64,
}

pub fn parse_smaps_rollup(text: &str) -> Result<SmapsRollup, ReadError> {
    let mut r = SmapsRollup::default();
    let mut seen_pss = false;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        match key {
            "Pss" => {
                r.pss = kb_line_bytes(val).ok_or_else(|| ReadError::parse("smaps_rollup: Pss"))?;
                seen_pss = true;
            }
            "Swap" => r.swap = kb_line_bytes(val).unwrap_or(0),
            "SwapPss" => r.swap_pss = kb_line_bytes(val).unwrap_or(0),
            _ => {}
        }
    }
    if !seen_pss {
        return Err(ReadError::parse("smaps_rollup: no Pss"));
    }
    Ok(r)
}
