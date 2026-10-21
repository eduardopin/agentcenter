//! Historico em memoria: um ring buffer por sessao, [`HISTORY_LEN`] amostras de
//! `f32` (10 min a 1 s). Sem persistencia em disco.
//!
//! Valor ausente na amostra (sem anterior, ilegivel) e guardado como `NaN`, para
//! a serie nao inventar um 0; quem desenha pula o ponto.

use crate::session::Session;
use std::collections::HashMap;

/// Amostras por serie: 10 min a 1 s (ARCHITECTURE, "Sampling").
pub const HISTORY_LEN: usize = 600;

/// Buffer circular de tamanho fixo: cheio, cada `push` descarta a mais antiga.
#[derive(Debug, Clone, PartialEq)]
pub struct Ring {
    buf: Vec<f32>,
    /// Indice da mais antiga quando cheio; com `buf` incompleto, sempre 0.
    head: usize,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            head: 0,
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, v: f32) {
        if self.buf.len() < self.cap {
            self.buf.push(v);
        } else {
            self.buf[self.head] = v;
            self.head = (self.head + 1) % self.cap;
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Da mais antiga para a mais nova.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = f32> + '_ {
        let (new, old) = self.buf.split_at(self.head);
        old.iter().chain(new).copied()
    }

    pub fn latest(&self) -> Option<f32> {
        self.iter().next_back()
    }

    /// Copia em ordem cronologica (para a GUI desenhar sem segurar o sampler).
    pub fn to_vec(&self) -> Vec<f32> {
        self.iter().collect()
    }
}

fn opt<T: Into<f64>>(v: Option<T>) -> f32 {
    v.map_or(f32::NAN, |x| x.into() as f32)
}

/// Series de uma sessao, todas com o mesmo comprimento.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionHistory {
    /// `starttime` do processo agente: pid reusado comeca historico novo.
    starttime: u64,
    /// % de um nucleo.
    pub cpu_pct: Ring,
    pub pss_bytes: Ring,
    pub swap_pss_bytes: Ring,
    pub read_bps: Ring,
    pub write_bps: Ring,
    /// `stall_pct` com delayacct; no fallback, numero de threads em D
    /// (ver `Machine::io_stall_source`).
    pub stall: Ring,
}

impl SessionHistory {
    fn new(starttime: u64, cap: usize) -> Self {
        Self {
            starttime,
            cpu_pct: Ring::new(cap),
            pss_bytes: Ring::new(cap),
            swap_pss_bytes: Ring::new(cap),
            read_bps: Ring::new(cap),
            write_bps: Ring::new(cap),
            stall: Ring::new(cap),
        }
    }

    pub fn len(&self) -> usize {
        self.cpu_pct.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cpu_pct.is_empty()
    }

    fn push(&mut self, s: &Session) {
        self.cpu_pct.push(opt(s.cpu_pct));
        // u64 -> f64 perde precisao acima de 2^53 bytes; f32 ja arredonda muito antes.
        self.pss_bytes.push(opt(s.pss_bytes.map(|b| b as f64)));
        self.swap_pss_bytes
            .push(opt(s.swap_pss_bytes.map(|b| b as f64)));
        self.read_bps.push(opt(s.read_bps));
        self.write_bps.push(opt(s.write_bps));
        self.stall.push(match (s.stall_pct, s.threads_d) {
            (Some(p), _) => p as f32,
            (None, Some(d)) => d as f32,
            (None, None) => f32::NAN,
        });
    }
}

/// Historico de todas as sessoes vivas, por pid do agente. Sessao que some
/// leva o historico junto.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    cap: usize,
    sessions: HashMap<u32, SessionHistory>,
}

impl Default for History {
    fn default() -> Self {
        Self::new(HISTORY_LEN)
    }
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            sessions: HashMap::new(),
        }
    }

    pub fn get(&self, agent_pid: u32) -> Option<&SessionHistory> {
        self.sessions.get(&agent_pid)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Acrescenta uma amostra por sessao e descarta as que nao estao em `sessions`.
    /// `starttime(pid)` identifica o processo agente para pegar pid reusado.
    pub fn record(&mut self, sessions: &[Session], starttime: impl Fn(u32) -> u64) {
        self.sessions
            .retain(|pid, _| sessions.iter().any(|s| s.pid == *pid));
        for s in sessions {
            let st = starttime(s.pid);
            let h = self
                .sessions
                .entry(s.pid)
                .or_insert_with(|| SessionHistory::new(st, self.cap));
            if h.starttime != st {
                *h = SessionHistory::new(st, self.cap);
            }
            h.push(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_and_601st_sample_drops_the_1st() {
        let mut r = Ring::new(HISTORY_LEN);
        for i in 0..HISTORY_LEN {
            r.push(i as f32);
        }
        assert_eq!(r.len(), HISTORY_LEN);
        assert_eq!(r.iter().next(), Some(0.0));
        r.push(600.0);
        assert_eq!(r.len(), HISTORY_LEN, "nao cresce alem da capacidade");
        let v = r.to_vec();
        assert_eq!(v[0], 1.0, "a 1a amostra saiu");
        assert_eq!(v[HISTORY_LEN - 1], 600.0);
        assert_eq!(r.latest(), Some(600.0));
        // Ordem cronologica mantida depois de varias voltas.
        for i in 601..1500 {
            r.push(i as f32);
        }
        let v = r.to_vec();
        assert!(v.windows(2).all(|w| w[1] == w[0] + 1.0));
        assert_eq!(v[0], 900.0);
    }

    #[test]
    fn ring_partial_fill_is_chronological() {
        let mut r = Ring::new(4);
        assert_eq!(r.latest(), None);
        r.push(1.0);
        r.push(2.0);
        assert_eq!(r.to_vec(), [1.0, 2.0]);
    }
}
