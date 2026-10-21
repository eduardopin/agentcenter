//! `/proc/<pid>/statm`: RSS por processo (mostrado, nunca somado).

use super::ReadError;

/// Tamanho de pagina assumido. Linux x86_64/aarch64 comum usa 4 KiB; o card nao
/// admite libc so para `sysconf(_SC_PAGESIZE)`.
pub const PAGE_SIZE: u64 = 4096;

/// RSS em bytes (segundo campo de `statm`, em paginas).
pub fn parse_statm_rss(text: &str) -> Result<u64, ReadError> {
    let pages = text
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| ReadError::parse("statm"))?;
    Ok(pages * PAGE_SIZE)
}
