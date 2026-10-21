//! Reclaim: sessoes ociosas, maior PSS + SwapPss primeiro, e quanto de memoria
//! sairia com elas. Regra pura sobre snapshots; quem chama faz os ticks e nunca
//! envia sinal daqui.
//!
//! O estado e o do resumo ([`session_state`]): `overlay.status` (Claude, ou
//! transcript de Codex/pi) primeiro, senao o status do pane herdr quando e um
//! valor reconhecido. Ociosa:
//! - estado `idle` (declarado por qualquer dos dois); ou
//! - estado desconhecido (sem `overlay.status` e sem status de pane
//!   reconhecido), CPU da sessao abaixo de [`IDLE_MAX_CPU_PCT`] em
//!   todos os ticks da janela ([`CpuWindow`]). A sessao precisa estar em todos
//!   os ticks com CPU conhecida e completa: pid novo, processo novo na sessao
//!   (soma parcial) ou tick perdido contam como nao observado, e nao entram.
//!
//! `busy` e `blocked`, do overlay ou do pane (`working`, `blocked`), nunca
//! entram, qualquer que seja a CPU. Tambem ficam de
//! fora o multiplexador (o `signal` recusa o servidor herdr), a sessao que
//! contem `own_pid` (quem chama: terminar derrubaria o proprio comando) e a
//! sessao sem PSS legivel (outro usuario: nao da para dizer quanto sai).

use crate::adapters::AgentRole;
use crate::overlay::SessionOverlay;
use crate::session::{Metric, Session};
use crate::snapshot::Snapshot;
use crate::summary::{SessionState, session_state};
use serde::Serialize;
use std::collections::HashMap;

/// CPU (% de um nucleo, soma da sessao) abaixo da qual a sessao sem status de
/// overlay conta como ociosa. O mesmo 1 % da sessao quieta do sampler.
pub const IDLE_MAX_CPU_PCT: f64 = 1.0;

/// Janela padrao de observacao, em segundos (um tick por segundo).
pub const DEFAULT_WINDOW_S: u64 = 10;

/// Por que a sessao entrou.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleBy {
    /// Estado declarado `idle`: `overlay.status` ou o status do pane herdr.
    Overlay,
    /// Estado desconhecido, CPU abaixo de [`IDLE_MAX_CPU_PCT`] na janela toda.
    Cpu,
}

/// CPU de cada sessao em cada tick com taxas, por pid do agente.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuWindow {
    /// Ticks com taxas observados.
    ticks: usize,
    /// Uma amostra por tick desde que a sessao apareceu sem interrupcao.
    /// `None`: CPU desconhecida ou parcial naquele tick.
    samples: HashMap<u32, Vec<Option<f64>>>,
}

impl CpuWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acrescenta um tick. Snapshot sem taxas (`interval_ms == 0`, o primeiro
    /// tick) e ignorado. Sessao ausente do tick perde o que tinha: a janela
    /// dela recomeca e fica mais curta que a das outras.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.interval_ms == 0 {
            return;
        }
        self.ticks += 1;
        self.samples
            .retain(|pid, _| snap.sessions.iter().any(|s| s.pid == *pid));
        for s in &snap.sessions {
            let cpu = s.cpu_pct.filter(|_| !s.partial.contains(&Metric::CpuPct));
            self.samples.entry(s.pid).or_default().push(cpu);
        }
    }

    /// Ticks com taxas observados ate agora.
    pub fn ticks(&self) -> usize {
        self.ticks
    }

    /// Maior CPU da sessao na janela. `None` se a janela esta vazia, se a
    /// sessao nao esteve em todos os ticks ou se algum tick nao tem CPU completa.
    pub fn max_cpu(&self, agent_pid: u32) -> Option<f64> {
        let v = self.samples.get(&agent_pid)?;
        if self.ticks == 0 || v.len() != self.ticks {
            return None;
        }
        v.iter().try_fold(f64::MIN, |m, c| c.map(|c| m.max(c)))
    }
}

/// Uma sessao ociosa, como estava no ultimo tick.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub pid: u32,
    pub agent: String,
    pub cwd: Option<String>,
    pub overlay: Option<SessionOverlay>,
    pub idle_by: IdleBy,
    /// Maior CPU da sessao na janela; `None` se nao foi observada inteira
    /// (so acontece com `idle_by == overlay`).
    pub max_cpu_pct: Option<f64>,
    pub pss_bytes: u64,
    /// `None` se ilegivel; conta 0 no total.
    pub swap_pss_bytes: Option<u64>,
    /// A soma de PSS ou SwapPss deixou algum processo da sessao de fora.
    pub memory_partial: bool,
}

impl Candidate {
    /// PSS + SwapPss: o que sai com a sessao.
    pub fn memory_bytes(&self) -> u64 {
        self.pss_bytes + self.swap_pss_bytes.unwrap_or(0)
    }
}

/// Sessoes ociosas, maior memoria primeiro, e a soma delas.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reclaim {
    /// Soma de [`Candidate::memory_bytes`] das linhas.
    pub total_bytes: u64,
    pub sessions: Vec<Candidate>,
}

/// Por que `s` e ociosa, ou `None` se nao e. Estado de [`session_state`]; so
/// o desconhecido cai na regra de CPU.
pub fn idle_by(s: &Session, window: &CpuWindow) -> Option<IdleBy> {
    match session_state(s) {
        SessionState::Idle => Some(IdleBy::Overlay),
        SessionState::Busy | SessionState::Blocked => None,
        SessionState::Unknown => window
            .max_cpu(s.pid)
            .filter(|&c| c < IDLE_MAX_CPU_PCT)
            .map(|_| IdleBy::Cpu),
    }
}

/// Sessoes ociosas de `snap` (o ultimo tick da janela). `own_pid`: pid de quem
/// chama; a sessao que o contem fica de fora.
pub fn reclaim(snap: &Snapshot, window: &CpuWindow, own_pid: Option<u32>) -> Reclaim {
    let mut sessions: Vec<Candidate> = snap
        .sessions
        .iter()
        .filter(|s| s.role == AgentRole::Agent)
        .filter(|s| own_pid.is_none_or(|me| !s.processes.iter().any(|p| p.pid == me)))
        .filter_map(|s| {
            let pss_bytes = s.pss_bytes?;
            let idle_by = idle_by(s, window)?;
            Some(Candidate {
                pid: s.pid,
                agent: s.agent.clone(),
                cwd: s.cwd.clone(),
                overlay: s.overlay.clone(),
                idle_by,
                max_cpu_pct: window.max_cpu(s.pid),
                pss_bytes,
                swap_pss_bytes: s.swap_pss_bytes,
                memory_partial: s.partial.contains(&Metric::PssBytes)
                    || s.partial.contains(&Metric::SwapPssBytes),
            })
        })
        .collect();
    sessions.sort_by_key(|c| (std::cmp::Reverse(c.memory_bytes()), c.pid));
    Reclaim {
        total_bytes: sessions.iter().map(Candidate::memory_bytes).sum(),
        sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{HerdrOverlay, OverlayStatus};
    use crate::session::ProcessMetrics;
    use crate::snapshot::{IoStallSource, Machine, SCHEMA_VERSION};

    const GB: u64 = 1 << 30;

    fn proc_(pid: u32, ppid: u32, cpu: Option<f64>, pss: Option<u64>) -> ProcessMetrics {
        ProcessMetrics {
            pid,
            ppid,
            comm: "claude".into(),
            state: 'S',
            uid: 1000,
            threads: 1,
            cpu_pct: cpu,
            rss_bytes: pss,
            pss_bytes: pss,
            swap_bytes: Some(0),
            swap_pss_bytes: Some(0),
            read_bps: Some(0.0),
            write_bps: Some(0.0),
            rchar_bps: Some(0.0),
            wchar_bps: Some(0.0),
            stall_pct: Some(0.0),
            threads_d: None,
            voluntary_ctxt_switches: 0,
            nonvoluntary_ctxt_switches: 0,
        }
    }

    fn session(pid: u32, cpu: Option<f64>, pss: u64, status: Option<OverlayStatus>) -> Session {
        let mut s = Session::aggregate(
            "claude".into(),
            AgentRole::Agent,
            pid,
            16,
            vec![proc_(pid, 1, cpu, Some(pss))],
        );
        s.overlay = status.map(|st| SessionOverlay {
            status: Some(st),
            ..Default::default()
        });
        s
    }

    fn snap(interval_ms: u64, sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            schema_version: SCHEMA_VERSION,
            taken_at_ms: 0,
            interval_ms,
            machine: Machine {
                cores: 16,
                cpu_pct: None,
                io_stall_source: IoStallSource::Delayacct,
                processes: sessions.len(),
                session_processes: sessions.len(),
                mem_total_bytes: None,
                mem_available_bytes: None,
                swap_total_bytes: None,
                swap_free_bytes: None,
                load_avg: None,
                psi: None,
            },
            sessions,
            summary: Default::default(),
        }
    }

    /// Janela de `ticks` ticks com os mesmos `sessions`, mais o tick sem taxas.
    fn window(ticks: usize, sessions: &[Session]) -> CpuWindow {
        let mut w = CpuWindow::new();
        w.observe(&snap(0, sessions.to_vec()));
        for _ in 0..ticks {
            w.observe(&snap(1000, sessions.to_vec()));
        }
        w
    }

    fn pids(r: &Reclaim) -> Vec<u32> {
        r.sessions.iter().map(|c| c.pid).collect()
    }

    #[test]
    fn reclaim_idle_2gb_comes_before_idle_1gb_and_total_is_the_sum() {
        let sessions = vec![
            session(10, Some(50.0), GB, Some(OverlayStatus::Idle)),
            session(20, Some(0.2), 2 * GB, None),
        ];
        let w = window(3, &sessions);
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert_eq!(pids(&r), [20, 10]);
        assert_eq!(r.sessions[0].idle_by, IdleBy::Cpu);
        assert_eq!(r.sessions[1].idle_by, IdleBy::Overlay);
        assert_eq!(r.total_bytes, 3 * GB);
        assert_eq!(
            r.total_bytes,
            r.sessions.iter().map(Candidate::memory_bytes).sum::<u64>()
        );
    }

    #[test]
    fn reclaim_busy_or_blocked_never_enters_even_with_zero_cpu() {
        let sessions = vec![
            session(10, Some(0.0), 4 * GB, Some(OverlayStatus::Busy)),
            session(20, Some(0.0), 3 * GB, Some(OverlayStatus::Blocked)),
            session(30, Some(0.0), GB, Some(OverlayStatus::Idle)),
        ];
        let w = window(10, &sessions);
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert_eq!(pids(&r), [30]);
        assert_eq!(r.total_bytes, GB);
    }

    #[test]
    fn reclaim_without_overlay_and_cpu_5pct_does_not_enter() {
        let sessions = vec![session(10, Some(5.0), 2 * GB, None)];
        let w = window(10, &sessions);
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert!(r.sessions.is_empty(), "{r:?}");
        assert_eq!(r.total_bytes, 0);
    }

    #[test]
    fn reclaim_cpu_rule_needs_every_tick_under_1pct() {
        // Um tick so acima de 1 % derruba a janela inteira.
        let mut w = window(4, &[session(10, Some(0.1), GB, None)]);
        let last = vec![session(10, Some(1.0), GB, None)];
        w.observe(&snap(1000, last.clone()));
        assert_eq!(w.max_cpu(10), Some(1.0));
        assert!(reclaim(&snap(1000, last), &w, None).sessions.is_empty());
        // Abaixo do limite em todos: entra, com o maior valor da janela.
        let ok = vec![session(10, Some(0.9), GB, None)];
        let mut w = window(4, &[session(10, Some(0.1), GB, None)]);
        w.observe(&snap(1000, ok.clone()));
        let r = reclaim(&snap(1000, ok), &w, None);
        assert_eq!(pids(&r), [10]);
        assert_eq!(r.sessions[0].max_cpu_pct, Some(0.9));
    }

    #[test]
    fn reclaim_cpu_rule_needs_the_session_observed_in_the_whole_window() {
        // Sem nenhum tick com taxas: nada e ocioso por CPU.
        let s = vec![session(10, Some(0.0), GB, None)];
        let mut w = CpuWindow::new();
        w.observe(&snap(0, s.clone()));
        assert_eq!(w.ticks(), 0);
        assert!(reclaim(&snap(0, s.clone()), &w, None).sessions.is_empty());

        // Sessao que aparece no meio da janela, ou some e volta: nao entra.
        let mut w = window(3, &[]);
        w.observe(&snap(1000, s.clone()));
        assert_eq!(w.max_cpu(10), None);
        let mut w = window(3, &s);
        w.observe(&snap(1000, vec![]));
        w.observe(&snap(1000, s.clone()));
        assert!(
            reclaim(&snap(1000, s.clone()), &w, None)
                .sessions
                .is_empty()
        );

        // CPU desconhecida num tick (pid reusado, sem anterior): nao entra.
        let mut w = window(3, &s);
        w.observe(&snap(1000, vec![session(10, None, GB, None)]));
        assert!(reclaim(&snap(1000, s), &w, None).sessions.is_empty());
    }

    #[test]
    fn reclaim_partial_cpu_counts_as_not_observed() {
        // Processo novo na sessao: a soma deixa ele de fora e marca parcial.
        let mut s = Session::aggregate(
            "claude".into(),
            AgentRole::Agent,
            10,
            16,
            vec![
                proc_(10, 1, Some(0.0), Some(GB)),
                proc_(11, 10, None, Some(GB)),
            ],
        );
        assert!(s.partial.contains(&Metric::CpuPct));
        let w = window(5, std::slice::from_ref(&s));
        assert_eq!(w.max_cpu(10), None);
        assert!(
            reclaim(&snap(1000, vec![s.clone()]), &w, None)
                .sessions
                .is_empty()
        );
        // Com status idle do overlay a regra de CPU nao vale.
        s.overlay = Some(SessionOverlay {
            status: Some(OverlayStatus::Idle),
            ..Default::default()
        });
        let r = reclaim(&snap(1000, vec![s]), &w, None);
        assert_eq!(pids(&r), [10]);
        assert_eq!(r.sessions[0].max_cpu_pct, None);
    }

    #[test]
    fn reclaim_overlay_without_status_falls_back_to_cpu() {
        let mut s = session(10, Some(0.0), GB, None);
        s.overlay = Some(SessionOverlay {
            name: Some("painel".into()),
            ..Default::default()
        });
        let w = window(2, std::slice::from_ref(&s));
        let r = reclaim(&snap(1000, vec![s]), &w, None);
        assert_eq!(pids(&r), [10]);
        assert_eq!(r.sessions[0].idle_by, IdleBy::Cpu);
    }

    /// Sessao num pane herdr com `status` do pane e sem `overlay.status` (o
    /// caso de Codex e pi sem estado no transcript).
    fn pane(pid: u32, cpu: f64, pss: u64, status: &str) -> Session {
        let mut s = session(pid, Some(cpu), pss, None);
        s.overlay = Some(SessionOverlay {
            herdr: Some(HerdrOverlay {
                status: Some(status.into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        s
    }

    #[test]
    fn reclaim_herdr_pane_state_decides_before_cpu() {
        let sessions = vec![
            // Pane blocked ou working, CPU 0: nunca entra.
            pane(10, 0.0, 4 * GB, "blocked"),
            pane(20, 0.0, 3 * GB, "working"),
            // Pane idle ou done: entra mesmo com CPU alta.
            pane(30, 50.0, 2 * GB, "idle"),
            pane(40, 50.0, GB, "done"),
            // Valor que o herdr nao publica: cai na regra de CPU.
            pane(50, 0.0, GB / 2, "thinking_v2"),
            pane(60, 5.0, GB / 4, "thinking_v2"),
        ];
        let w = window(3, &sessions);
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert_eq!(pids(&r), [30, 40, 50], "{r:?}");
        let by: Vec<IdleBy> = r.sessions.iter().map(|c| c.idle_by).collect();
        assert_eq!(by, [IdleBy::Overlay, IdleBy::Overlay, IdleBy::Cpu]);
    }

    #[test]
    fn reclaim_leaves_out_multiplexer_own_session_and_unreadable_memory() {
        let mut herdr = session(5, Some(0.0), 8 * GB, Some(OverlayStatus::Idle));
        herdr.role = AgentRole::Multiplexer;
        // Quem chama roda dentro da sessao 10 (pid 12 e filho dela).
        let own = Session::aggregate(
            "claude".into(),
            AgentRole::Agent,
            10,
            16,
            vec![
                proc_(10, 1, Some(0.0), Some(GB)),
                proc_(12, 10, Some(0.0), Some(1)),
            ],
        );
        let mut other_user = session(20, Some(0.0), GB, Some(OverlayStatus::Idle));
        other_user.pss_bytes = None;
        let ok = session(30, Some(0.0), GB / 2, Some(OverlayStatus::Idle));
        let sessions = vec![herdr, own, other_user, ok];
        let w = window(2, &sessions);
        let r = reclaim(&snap(1000, sessions.clone()), &w, Some(12));
        assert_eq!(pids(&r), [30]);
        assert_eq!(r.total_bytes, GB / 2);
        // Sem own_pid a sessao 10 entra.
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert_eq!(pids(&r), [10, 30]);
    }

    #[test]
    fn reclaim_swap_counts_in_the_order_and_the_total() {
        let mut a = session(10, Some(0.0), GB, Some(OverlayStatus::Idle));
        a.swap_pss_bytes = Some(2 * GB);
        let mut b = session(20, Some(0.0), 2 * GB, Some(OverlayStatus::Idle));
        b.swap_pss_bytes = None;
        b.partial.push(Metric::SwapPssBytes);
        let sessions = vec![b, a];
        let w = window(1, &sessions);
        let r = reclaim(&snap(1000, sessions), &w, None);
        assert_eq!(pids(&r), [10, 20]);
        assert_eq!(r.total_bytes, 5 * GB);
        assert!(!r.sessions[0].memory_partial);
        assert!(r.sessions[1].memory_partial);
    }
}
