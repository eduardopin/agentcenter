//! Explicit user-initiated control. Never called by the sampler or GTK tick.
//!
//! Common contract, agent-agnostic: the operation states and the capability
//! shape are the ones of the Pi protocol 2 (re-exported by [`pi`] unchanged on
//! the wire); an operation is named by a client-chosen UUID; codes are closed
//! sets of `&'static str`; evidence carries path, time, size and sha256, never
//! content. Adapters: [`pi`] (protocol 2 socket) and [`claude`] (Claude Code
//! through Herdr). [`pickup`] only declares the composite handoff-pickup stages.
use serde::{Deserialize, Serialize};
use std::{io::Read, path::PathBuf};

pub mod claude;
pub mod herdr;
pub mod pi;
pub mod pickup;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Accepted,
    Running,
    Completed,
    Rejected,
    Failed,
    Cancelled,
    Unknown,
}
impl State {
    /// Unknown is not terminal: a later status with the same operation ID may
    /// still reconcile it from evidence.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    #[serde(default)]
    pub cancel_supported: bool,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}
impl Capability {
    pub fn enabled() -> Self {
        Self {
            cancel_supported: false,
            enabled: true,
            disabled_reason: None,
        }
    }
    pub fn disabled(code: &'static str) -> Self {
        Self {
            cancel_supported: false,
            enabled: false,
            disabled_reason: Some(code.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Handoff,
    Compact,
}

/// Persisted handoff file: identity and digest only. The bytes are hashed and
/// dropped; nothing here may hold or display the file's text.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Artifact {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub modified_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Evidence {
    /// Transcript `compact_boundary` (manual) time, epoch ms.
    Compacted {
        boundary_ms: u64,
    },
    Persisted(Artifact),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Outcome {
    pub operation_id: String,
    pub action: Action,
    pub state: State,
    pub code: &'static str,
    pub evidence: Option<Evidence>,
}

/// Local telemetry record: no prompt, text, transcript path or session ID.
/// `correlation` is a truncated sha256 of the operation ID, opaque by design.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Telemetry {
    pub action: Action,
    pub code: &'static str,
    pub duration_ms: u64,
    pub correlation: String,
}
impl Telemetry {
    pub fn new(action: Action, code: &'static str, duration_ms: u64, operation_id: &str) -> Self {
        Self {
            action,
            code,
            duration_ms,
            correlation: hex(&sha256(operation_id.as_bytes())[..8]),
        }
    }
}

/// Lowercase canonical UUID text (8-4-4-4-12 hex).
pub fn operation_id_valid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// FIPS 180-4 SHA-256 over a stream (the crate has no hashing dependency).
/// Returns the digest and the number of bytes read.
pub(crate) fn sha256_reader(mut r: impl Read) -> std::io::Result<([u8; 32], u64)> {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let block = |h: &mut [u32; 8], b: &[u8]| {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b2, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..64 {
            let t1 = hh
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b2) ^ (a & c) ^ (b2 & c));
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b2;
            b2 = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b2, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    };
    let mut buf = [0u8; 64];
    let mut fill = 0;
    let mut total: u64 = 0;
    loop {
        let n = match r.read(&mut buf[fill..]) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        fill += n;
        total += n as u64;
        if fill == 64 {
            block(&mut h, &buf);
            fill = 0;
        }
    }
    let bits = total.wrapping_mul(8);
    buf[fill] = 0x80;
    buf[fill + 1..].fill(0);
    if fill >= 56 {
        block(&mut h, &buf);
        buf.fill(0);
    }
    buf[56..].copy_from_slice(&bits.to_be_bytes());
    block(&mut h, &buf);
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    Ok((out, total))
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    sha256_reader(bytes).map(|(d, _)| d).unwrap_or([0; 32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_fips_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let two = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            hex(&sha256(two)),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha256(&million)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn operation_ids_are_canonical_lowercase_uuids() {
        assert!(operation_id_valid("0b0e7c1a-1111-4aaa-8bbb-0123456789ab"));
        assert!(!operation_id_valid("0B0E7C1A-1111-4AAA-8BBB-0123456789AB"));
        assert!(!operation_id_valid("0b0e7c1a11114aaa8bbb0123456789ab"));
        assert!(!operation_id_valid("../../../../etc/passwd-aaaaaaaaaaaaa"));
    }

    #[test]
    fn telemetry_correlation_is_opaque() {
        let id = "0b0e7c1a-1111-4aaa-8bbb-0123456789ab";
        let t = Telemetry::new(Action::Compact, "compacted", 7, id);
        assert_eq!(t.correlation.len(), 16);
        assert!(!t.correlation.contains("0b0e7c1a"));
        assert_eq!(t, Telemetry::new(Action::Compact, "compacted", 7, id));
    }
}
