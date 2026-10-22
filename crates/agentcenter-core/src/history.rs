//! Historico em memoria: um ring buffer por sessao, [`HISTORY_LEN`] amostras de
//! `f32` (10 min a 1 s). Sem persistencia em disco.
//!
//! Valor ausente na amostra (sem anterior, ilegivel) e guardado como `NaN`, para
//! a serie nao inventar um 0; quem desenha pula o ponto.
//!
//! Series de cockpit (CC9): copiadas do `Session.overlay` que o sampler ja
//! preencheu no tick (`History::record` roda depois de `overlay::apply`), sem
//! leitura nova. Sem overlay, sem `cockpit` ou com `cockpit_pending` a amostra
//! e `NaN`: varredura incompleta nao e "0 % de burn".

use crate::cockpit::Loops;
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
    /// `overlay.context_pct`: % da janela de contexto.
    pub ctx_pct: Ring,
    /// `overlay.cockpit.reread_share`: parcela de releitura, de 0 a 1.
    pub reread_share: Ring,
    /// `overlay.cockpit.burn_pct`: burn em % do orcamento da sessao.
    pub burn_pct: Ring,
    /// `overlay.tool_calls_per_min` (ultimos 5 min do transcript).
    pub tool_calls_per_min: Ring,
    /// [`Loops::repeats`] do cockpit: a maior repeticao quando passa de
    /// [`crate::cockpit::LOOP_REPEATS`], 0 com o sinal lido e sem loop, `NaN`
    /// sem sinal (`max_streak` 0: cockpit sem a leitura de loops).
    pub loop_repeats: Ring,
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
            ctx_pct: Ring::new(cap),
            reread_share: Ring::new(cap),
            burn_pct: Ring::new(cap),
            tool_calls_per_min: Ring::new(cap),
            loop_repeats: Ring::new(cap),
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
        let o = s.overlay.as_ref();
        self.ctx_pct.push(opt(o.and_then(|o| o.context_pct)));
        self.tool_calls_per_min
            .push(opt(o.and_then(|o| o.tool_calls_per_min)));
        let c = o
            .filter(|o| !o.cockpit_pending)
            .and_then(|o| o.cockpit.as_ref());
        self.reread_share.push(opt(c.map(|c| c.reread_share)));
        self.burn_pct.push(opt(c.map(|c| c.burn_pct)));
        self.loop_repeats.push(opt(c.and_then(|c| {
            let l = Loops {
                max_streak: c.max_streak,
                repeated_max: c.repeated_max,
            };
            // max_streak 0 e "sem sinal" (Loops::default), nunca "sem loop".
            (l.max_streak > 0).then(|| l.repeats().unwrap_or(0))
        })));
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

    // ---- Series de cockpit (CC9) ----------------------------------------

    use crate::adapters::AgentRole;
    use crate::cockpit::{Cockpit, LOOP_REPEATS, Loops, Thresholds, Tokens};
    use crate::overlay::SessionOverlay;

    fn agent(pid: u32) -> Session {
        Session::aggregate("claude".into(), AgentRole::Agent, pid, 16, Vec::new())
    }

    fn cockpit(loops: Loops) -> Cockpit {
        let t = Tokens {
            input: 1_000_000,
            cache_write: 200_000,
            cache_read: 3_000_000,
            output: 50_000,
            ctx: 600_000,
            window: 1_000_000,
            ..Default::default()
        };
        Cockpit::compute_with_loops(&Thresholds::default(), &t, loops, None, None)
    }

    fn with_overlay(pid: u32, o: SessionOverlay) -> Session {
        let mut s = agent(pid);
        s.overlay = Some(o);
        s
    }

    fn latest(r: &Ring) -> f32 {
        r.latest().expect("one sample")
    }

    fn cockpit_rings(h: &SessionHistory) -> [&Ring; 3] {
        [&h.reread_share, &h.burn_pct, &h.loop_repeats]
    }

    #[test]
    fn cockpit_rings_are_nan_without_overlay_cockpit_or_while_pending() {
        let mut h = History::new(HISTORY_LEN);
        // Sem overlay nenhum: todas as series de cockpit e do transcript ausentes.
        h.record(&[agent(1)], |_| 7);
        let s = h.get(1).unwrap();
        for r in cockpit_rings(s)
            .into_iter()
            .chain([&s.ctx_pct, &s.tool_calls_per_min])
        {
            assert!(latest(r).is_nan(), "sem overlay e NaN, nunca 0");
        }

        // Overlay com contexto e tool calls, cockpit ainda em varredura.
        let pending = SessionOverlay {
            context_pct: Some(42.5),
            tool_calls_per_min: Some(3.2),
            cockpit_pending: true,
            ..Default::default()
        };
        // Pendente mesmo com um cockpit velho no overlay: nao e leitura desta varredura.
        let stale = SessionOverlay {
            cockpit: Some(cockpit(Loops::default())),
            ..pending.clone()
        };
        for o in [pending, stale] {
            let mut h = History::new(HISTORY_LEN);
            h.record(&[with_overlay(2, o)], |_| 7);
            let s = h.get(2).unwrap();
            assert_eq!(latest(&s.ctx_pct), 42.5);
            assert_eq!(latest(&s.tool_calls_per_min), 3.2);
            for r in cockpit_rings(s) {
                assert!(latest(r).is_nan(), "pending e NaN, nunca 0");
            }
        }
    }

    #[test]
    fn cockpit_rings_take_the_overlay_values() {
        let c = cockpit(Loops {
            max_streak: 1,
            repeated_max: 2,
        });
        let (share, burn) = (c.reread_share as f32, c.burn_pct as f32);
        assert!(share > 0.0 && burn > 0.0, "fixture com valores reais");
        let o = SessionOverlay {
            context_pct: Some(60.0),
            tool_calls_per_min: Some(0.0),
            cockpit: Some(c),
            ..Default::default()
        };
        let mut h = History::new(HISTORY_LEN);
        h.record(&[with_overlay(3, o)], |_| 7);
        let s = h.get(3).unwrap();
        assert_eq!(latest(&s.ctx_pct), 60.0);
        assert_eq!(latest(&s.reread_share), share);
        assert_eq!(latest(&s.burn_pct), burn);
        assert_eq!(latest(&s.tool_calls_per_min), 0.0, "0/min lido e 0");
        assert_eq!(latest(&s.loop_repeats), 0.0, "sinal lido, sem loop: 0");
        assert_eq!(s.loop_repeats.len(), s.len(), "mesmo comprimento");
    }

    #[test]
    fn loop_without_signal_is_not_loop_zero() {
        let sample = |loops: Loops| {
            let o = SessionOverlay {
                cockpit: Some(cockpit(loops)),
                ..Default::default()
            };
            let mut h = History::new(HISTORY_LEN);
            h.record(&[with_overlay(4, o)], |_| 7);
            latest(&h.get(4).unwrap().loop_repeats)
        };
        // Loops::default (max_streak 0) e o cockpit sem a leitura de loops.
        assert!(sample(Loops::default()).is_nan());
        assert_eq!(
            sample(Loops {
                max_streak: 1,
                repeated_max: 0
            }),
            0.0
        );
        assert_eq!(
            sample(Loops {
                max_streak: 2,
                repeated_max: LOOP_REPEATS + 2
            }),
            (LOOP_REPEATS + 2) as f32,
            "a maior repeticao"
        );
        assert_eq!(
            sample(Loops {
                max_streak: LOOP_REPEATS,
                repeated_max: 1
            }),
            LOOP_REPEATS as f32
        );
    }

    #[test]
    fn reused_pid_resets_the_cockpit_rings_and_601st_drops_the_1st() {
        let mut h = History::new(HISTORY_LEN);
        let mut o = SessionOverlay::default();
        for i in 0..HISTORY_LEN {
            o.context_pct = Some(i as f64);
            h.record(&[with_overlay(5, o.clone())], |_| 7);
        }
        let s = h.get(5).unwrap();
        assert_eq!(s.ctx_pct.len(), HISTORY_LEN);
        assert_eq!(s.ctx_pct.iter().next(), Some(0.0));
        o.context_pct = Some(600.0);
        h.record(&[with_overlay(5, o.clone())], |_| 7);
        let v = h.get(5).unwrap().ctx_pct.to_vec();
        assert_eq!(v.len(), HISTORY_LEN);
        assert_eq!(
            (v[0], v[HISTORY_LEN - 1]),
            (1.0, 600.0),
            "a 1a amostra saiu"
        );

        // Mesmo pid, outro processo (starttime novo): historico novo, sem o passado.
        o.context_pct = Some(12.0);
        h.record(&[with_overlay(5, o)], |_| 8);
        let s = h.get(5).unwrap();
        for r in [&s.ctx_pct, &s.reread_share, &s.loop_repeats, &s.cpu_pct] {
            assert_eq!(r.len(), 1, "pid reusado comeca do zero");
        }
        assert_eq!(latest(&s.ctx_pct), 12.0);
    }
}
