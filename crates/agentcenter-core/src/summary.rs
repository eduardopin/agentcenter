//! Resumo de um tick: contagens e somas sobre as sessoes ja montadas.
//!
//! So le o [`Session`] que o sampler acabou de montar (com overlay aplicado):
//! nenhuma leitura de `/proc` nem de arquivo. As medidas de processo sao as que
//! a sessao publica, entao herdam a cadencia do sampler: numa sessao quieta o
//! estado e a CPU dos processos podem ter ate 5 s (ver o topo de sampler.rs).

use crate::adapters::AgentRole;
use crate::cockpit::{Recommendation, round_n};
use crate::overlay::OverlayStatus;
use crate::session::Session;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// CPU (% de um nucleo) a partir da qual um processo filho conta como trabalhando
/// mesmo fora de `R`/`D`: abaixo de 1 % e ruido de processo dormindo.
pub const WORKING_CPU_PCT: f64 = 1.0;

/// Estado de uma sessao de agente no resumo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Busy,
    Blocked,
    Idle,
    Unknown,
}

/// Sessoes de agente por estado; a soma e [`Summary::sessions`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusCounts {
    pub busy: usize,
    pub blocked: usize,
    pub idle: usize,
    pub unknown: usize,
}

/// Sessoes de agente pela recomendacao do cockpit; a soma e
/// [`Summary::sessions`] (schema 5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CockpitCounts {
    pub continuar: usize,
    pub compactar: usize,
    pub handoff: usize,
    pub ociosa: usize,
    #[serde(rename = "loop")]
    pub loop_: usize,
    /// Sem `overlay.cockpit`: sem transcript casado, transcript sem pedido
    /// contado, ou varredura ainda incompleta (estas tambem em
    /// [`Summary::cockpit_pending`]).
    pub sem_dado: usize,
}

/// Contagens do tick. Campos de sessao (`sessions`, `by_agent`, `status`,
/// `sessions_with_working_children`) contam so as sessoes de agente; os de
/// processo e as somas cobrem todas as sessoes (agentes e multiplexadores),
/// como a linha TOTAL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    /// Sessoes com `role` agente.
    pub sessions: usize,
    /// Sessoes com `role` multiplexador (herdr).
    pub multiplexers: usize,
    /// Sessoes de agente por id do adapter; a soma e `sessions`.
    pub by_agent: BTreeMap<String, usize>,
    pub status: StatusCounts,
    /// Sessoes de agente com ao menos um processo filho trabalhando (no
    /// criterio de `working_child_processes`). E medido no `/proc`, nao
    /// declarado: nao e `status.busy` (estado que o agente ou o herdr publica)
    /// e as duas contagens podem divergir nos dois sentidos (agente `busy`
    /// pensando sem filho; agente `idle` com um build de fundo rodando).
    pub sessions_with_working_children: usize,
    /// Processos de sessao fora o processo agente de cada uma.
    pub child_processes: usize,
    /// Dos `child_processes`, os em `R` ou `D` ou com CPU >= [`WORKING_CPU_PCT`].
    pub working_child_processes: usize,
    /// Sessoes com a flag `io_bound` (agentes e multiplexadores).
    pub io_bound_sessions: usize,
    /// Somas das sessoes que tem o valor; `None` se nenhuma tem.
    pub sessions_cpu_pct: Option<f64>,
    pub sessions_pss_bytes: Option<u64>,
    pub sessions_swap_pss_bytes: Option<u64>,
    /// Somas de `overlay.tool_calls_open` e `overlay.tool_calls_per_min`
    /// (overlay de transcripts) das sessoes que tem o valor; `None` se nenhuma.
    pub tool_calls_open: Option<u64>,
    pub tool_calls_per_min: Option<f64>,
    /// Soma de `overlay.subagents_live` (overlay de transcripts, C15) das
    /// sessoes que tem o valor; `None` se nenhuma.
    pub subagents_live: Option<u64>,
    /// Sessoes de agente por recomendacao do cockpit (schema 5).
    pub cockpit: CockpitCounts,
    /// Das sessoes de agente em `cockpit.sem_dado`, as com transcript casado
    /// e varredura incompleta (`overlay.cockpit_pending`).
    pub cockpit_pending: usize,
    /// Soma de `cockpit.burn_weighted` das sessoes de agente; `None` se
    /// nenhuma tem cockpit.
    pub sessions_burn_weighted: Option<u64>,
    /// Soma de `cockpit.cache_read` / soma de `overlay.tokens_in`, so das
    /// sessoes de agente com cockpit, 3 casas; `None` sem tokens. Ponderado
    /// pelos tokens, nao a media dos `cache_hit` de cada sessao.
    pub cache_hit: Option<f64>,
    /// Maior `overlay.context_pct` (1 casa, como publicado) das sessoes de
    /// agente; `None` se nenhuma tem.
    pub ctx_pct_max: Option<f64>,
}

/// Estado da sessao: `overlay.status` (o do Claude, ou o do transcript de
/// Codex/pi) vale sobre o status do pane herdr, que so conta se e um valor
/// reconhecido ([`crate::overlay::herdr_state`]); sem nenhum dos dois e `Unknown`. CPU nao
/// entra: ocioso nao se infere. O reclaim usa o mesmo estado.
pub fn session_state(s: &Session) -> SessionState {
    let declared = s
        .overlay
        .as_ref()
        .and_then(|o| o.status.or_else(|| o.herdr.as_ref()?.state()));
    match declared {
        Some(OverlayStatus::Busy) => SessionState::Busy,
        Some(OverlayStatus::Blocked) => SessionState::Blocked,
        Some(OverlayStatus::Idle) => SessionState::Idle,
        None => SessionState::Unknown,
    }
}

fn add<T: std::ops::Add<Output = T>>(acc: Option<T>, v: Option<T>) -> Option<T> {
    match (acc, v) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

impl Summary {
    /// Resumo das sessoes de um tick. Funcao pura: nada alem de `sessions`.
    pub fn of(sessions: &[Session]) -> Self {
        let mut sum = Summary::default();
        // Somas do cache_hit agregado, em u128 (nao satura).
        let (mut read, mut tokens_in) = (0u128, 0u128);
        for s in sessions {
            let mut working = 0;
            for p in s.processes.iter().filter(|p| p.pid != s.pid) {
                sum.child_processes += 1;
                if matches!(p.state, 'R' | 'D') || p.cpu_pct.is_some_and(|c| c >= WORKING_CPU_PCT) {
                    working += 1;
                }
            }
            sum.working_child_processes += working;
            sum.io_bound_sessions += usize::from(s.io_bound);
            sum.sessions_cpu_pct = add(sum.sessions_cpu_pct, s.cpu_pct);
            sum.sessions_pss_bytes = add(sum.sessions_pss_bytes, s.pss_bytes);
            sum.sessions_swap_pss_bytes = add(sum.sessions_swap_pss_bytes, s.swap_pss_bytes);
            if let Some(o) = &s.overlay {
                let open = sum.tool_calls_open;
                sum.tool_calls_open = match (open, o.tool_calls_open) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
                sum.tool_calls_per_min = add(sum.tool_calls_per_min, o.tool_calls_per_min);
                sum.subagents_live = match (sum.subagents_live, o.subagents_live) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
            }
            match s.role {
                AgentRole::Multiplexer => sum.multiplexers += 1,
                AgentRole::Agent => {
                    sum.sessions += 1;
                    match sum.by_agent.get_mut(s.agent.as_str()) {
                        Some(n) => *n += 1,
                        None => {
                            sum.by_agent.insert(s.agent.clone(), 1);
                        }
                    }
                    let st = &mut sum.status;
                    match session_state(s) {
                        SessionState::Busy => st.busy += 1,
                        SessionState::Blocked => st.blocked += 1,
                        SessionState::Idle => st.idle += 1,
                        SessionState::Unknown => st.unknown += 1,
                    }
                    sum.sessions_with_working_children += usize::from(working > 0);
                    let o = s.overlay.as_ref();
                    let c = &mut sum.cockpit;
                    match o.and_then(|o| o.cockpit.as_ref()) {
                        None => {
                            c.sem_dado += 1;
                            sum.cockpit_pending +=
                                usize::from(o.is_some_and(|o| o.cockpit_pending));
                        }
                        Some(k) => {
                            match k.recommendation {
                                Recommendation::Continuar => c.continuar += 1,
                                Recommendation::Compactar => c.compactar += 1,
                                Recommendation::Handoff => c.handoff += 1,
                                Recommendation::Ociosa => c.ociosa += 1,
                                Recommendation::Loop => c.loop_ += 1,
                            }
                            let burn = sum.sessions_burn_weighted.unwrap_or(0);
                            sum.sessions_burn_weighted = Some(burn.saturating_add(k.burn_weighted));
                            if let Some(t) = o.and_then(|o| o.tokens_in) {
                                read += u128::from(k.cache_read);
                                tokens_in += u128::from(t);
                            }
                        }
                    }
                    if let Some(p) = o.and_then(|o| o.context_pct) {
                        sum.ctx_pct_max = Some(sum.ctx_pct_max.map_or(p, |m: f64| m.max(p)));
                    }
                }
            }
        }
        sum.cache_hit = (tokens_in > 0).then(|| round_n(read as f64 / tokens_in as f64, 3));
        sum
    }
}
