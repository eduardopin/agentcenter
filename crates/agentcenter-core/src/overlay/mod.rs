//! Contrato de overlay: contexto que nao possui processos (herdr, estado do
//! Claude, transcripts). Os cards de overlay implementam [`Overlay`] e se
//! registram em [`registry`]; o modelo de sessao nao muda.

pub mod claude_state;
pub mod herdr;
pub mod transcript;

use crate::cockpit::Cockpit;
use crate::sampler::SamplerConfig;
use crate::session::Session;
use serde::{Deserialize, Serialize};

/// Estado do agente segundo o overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayStatus {
    Busy,
    Idle,
    Blocked,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HerdrOverlay {
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
}

impl HerdrOverlay {
    /// Estado declarado pelo pane, se o valor e um dos que o herdr publica
    /// (ver [`herdr_state`]).
    pub fn state(&self) -> Option<OverlayStatus> {
        self.status.as_deref().and_then(herdr_state)
    }
}

/// Status de agente que o herdr publica no pane, como estado: `working` e
/// busy, `blocked` e blocked, `idle` e `done` sao idle. Qualquer outro valor
/// (um que o herdr venha a publicar) e `None`: nao e estado declarado, e quem
/// sabe mais (o transcript) pode preencher. Unica lista dos valores do herdr:
/// o resumo, o reclaim e o overlay de transcripts passam por aqui.
pub fn herdr_state(status: &str) -> Option<OverlayStatus> {
    match status {
        "working" => Some(OverlayStatus::Busy),
        "blocked" => Some(OverlayStatus::Blocked),
        "idle" | "done" => Some(OverlayStatus::Idle),
        _ => None,
    }
}

/// Tudo opcional: cada overlay preenche o que sabe.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionOverlay {
    pub name: Option<String>,
    pub agent_session_id: Option<String>,
    pub status: Option<OverlayStatus>,
    pub kind: Option<String>,
    pub herdr: Option<HerdrOverlay>,
    pub model: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub context_pct: Option<f64>,
    /// Tool calls do transcript (overlay `transcript`): total, sem resultado
    /// e por minuto (ultimos 5 min).
    pub tool_calls: Option<u64>,
    pub tool_calls_open: Option<u64>,
    pub tool_calls_per_min: Option<f64>,
    /// Subagentes da sessao (overlay `transcript`, C15): vivos e total
    /// (Claude: `<sid>/subagents/agent-*.jsonl`; Codex: rollouts com
    /// `thread_spawn` do pai). `None` sem transcript casado ou no pi; vivos
    /// `None` enquanto a primeira leitura dos arquivos nao terminou.
    pub subagents_live: Option<u64>,
    pub subagents_total: Option<u64>,
    /// Provider canonico ([`crate::cockpit::provider`]): o declarado pelo
    /// transcript, senao o do agente, senao o prefixo do modelo (schema 5).
    pub provider: Option<String>,
    /// Metricas de cockpit (overlay `transcript`), so depois da varredura
    /// completa do transcript, como `tokens_in` (schema 5).
    pub cockpit: Option<Cockpit>,
    /// Transcript casado com a varredura ainda incompleta: `cockpit` e `None`
    /// por enquanto, nao por falta de dado (schema 5; `false` no JSON v4).
    #[serde(default)]
    pub cockpit_pending: bool,
}

/// Fonte de overlay. Recebe a sessao ja medida e completa `overlay`.
pub trait Overlay: Send + Sync {
    fn id(&self) -> &str;
    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay);
    /// A configuracao do sampler mudou (e na criacao dele); vale a partir do
    /// proximo tick. O overlay de transcripts le dela o orcamento de leitura.
    fn configure(&self, _config: &SamplerConfig) {}
}

/// Overlays ativos.
pub fn registry() -> Vec<Box<dyn Overlay>> {
    vec![
        Box::new(claude_state::ClaudeState::from_env()),
        Box::new(herdr::Herdr::from_env()),
        Box::new(transcript::Transcripts::from_env()),
    ]
}

/// Aplica os overlays; sessao sem nada preenchido fica com `overlay: None`.
pub fn apply(overlays: &[Box<dyn Overlay>], sessions: &mut [Session]) {
    if overlays.is_empty() {
        return;
    }
    for s in sessions {
        let mut o = s.overlay.take().unwrap_or_default();
        for ov in overlays {
            ov.enrich(s, &mut o);
        }
        s.overlay = (o != SessionOverlay::default()).then_some(o);
    }
}
