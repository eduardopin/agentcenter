//! Texto das linhas e chaves de ordenacao, sem GTK: uma [`Session`] vira uma
//! [`Row`] pronta para o modelo. Testavel sem display.
//!
//! Ausente sai travessao na lista ([`DASH`]) e `-` na faixa da maquina
//! ([`ABSENT`]); nunca 0, que e uma medida. Soma que nao cobre todos os
//! processos da sessao (`Session::partial`) sai com `≥`: o que faltou so pode
//! somar mais, entao o numero mostrado e um piso. Chave de ordenacao de valor
//! ausente e -1, abaixo de qualquer medida.

use agentcenter_core::adapters::AgentRole;
use agentcenter_core::cockpit::{self, Family, Reason, Recommendation};
use agentcenter_core::overlay::SessionOverlay;
use agentcenter_core::session::Metric;
use agentcenter_core::{IoStallSource, Session, SessionState};
use std::borrow::Cow;
use std::fmt::Write;

/// Ausente na faixa da maquina (machine_header.rs).
pub const ABSENT: &str = "-";
/// Ausente na lista de sessoes.
pub const DASH: &str = "\u{2014}";

/// Bytes em unidade IEC: `512 B`, `12.0 KiB`, `340 MiB`, `1.2 GiB`.
pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b.max(0.0);
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `used of total`, os dois na unidade do total: `21.3 of 38.0 GiB`.
pub fn used_of(used: u64, total: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut div = 1.0;
    let mut u = 0;
    while total as f64 / div >= 1024.0 && u < UNITS.len() - 1 {
        div *= 1024.0;
        u += 1;
    }
    let (a, b) = (used as f64 / div, total as f64 / div);
    if u == 0 {
        format!("{a:.0} of {b:.0} {}", UNITS[u])
    } else {
        format!("{a:.1} of {b:.1} {}", UNITS[u])
    }
}

/// Caracteres de controle e de formato (bidi, largura zero) escapados: cwd e
/// overlay vem de fora, e U+202E inverteria o resto do rotulo.
pub fn sanitize(s: &str) -> Cow<'_, str> {
    let bad = |c: char| {
        c.is_control()
            || matches!(c as u32, 0x00AD | 0x061C | 0x180E | 0x200B..=0x200F
                | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
    };
    if !s.chars().any(bad) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if bad(c) {
            let _ = write!(out, "\\u{{{:x}}}", c as u32);
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// `c` e `home` ou fica dentro dele vira `~`/`~/...`. `home` vazio nunca abrevia.
pub fn short_path(c: &str, home: Option<&str>) -> String {
    if let Some(h) = home.map(|h| h.trim_end_matches('/'))
        && !h.is_empty()
        && let Some(rest) = c.strip_prefix(h)
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("~{rest}");
    }
    c.to_string()
}

/// Grupo da linha no rack: o estado da sessao de agente (bloqueadas primeiro,
/// docs/DESIGN.md secao 5) e os multiplexadores (herdr) no fim, a parte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Group {
    Blocked,
    Busy,
    Idle,
    #[default]
    Unknown,
    Multiplexer,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Blocked,
        Group::Busy,
        Group::Idle,
        Group::Unknown,
        Group::Multiplexer,
    ];

    pub fn of(s: &Session) -> Self {
        if s.role == AgentRole::Multiplexer {
            return Group::Multiplexer;
        }
        match agentcenter_core::summary::session_state(s) {
            SessionState::Blocked => Group::Blocked,
            SessionState::Busy => Group::Busy,
            SessionState::Idle => Group::Idle,
            SessionState::Unknown => Group::Unknown,
        }
    }

    /// Posicao no rack, e o valor da propriedade `group`.
    pub fn rank(self) -> u32 {
        self as u32
    }

    pub fn from_rank(r: u32) -> Self {
        Self::ALL.get(r as usize).copied().unwrap_or_default()
    }

    /// Titulo da secao.
    pub fn title(self) -> &'static str {
        match self {
            Group::Blocked => "Blocked",
            Group::Busy => "Busy",
            Group::Idle => "Idle",
            Group::Unknown => "Unknown",
            Group::Multiplexer => "Multiplexers",
        }
    }
}

/// Recomendacao do cockpit como codigo da propriedade `rec`: sem dado,
/// varredura em curso, ou o veredito do script.
pub const REC_NONE: u32 = 0;
pub const REC_PENDING: u32 = 1;

pub fn rec_code(v: Recommendation) -> u32 {
    match v {
        Recommendation::Continuar => 2,
        Recommendation::Compactar => 3,
        Recommendation::Handoff => 4,
        Recommendation::Ociosa => 5,
        Recommendation::Loop => 6,
    }
}

pub const REC_HANDOFF: u32 = 4;

/// Texto da pilula: os nomes do script, `OK` para continuar.
pub fn rec_text(code: u32) -> &'static str {
    match code {
        REC_PENDING => "lendo",
        2 => "OK",
        3 => "COMPACTAR",
        4 => "HANDOFF",
        5 => "OCIOSA",
        6 => "LOOP",
        _ => DASH,
    }
}

/// Classe CSS da pilula: erro (HANDOFF, LOOP) preenchida de carmim, aviso
/// (COMPACTAR) contorno carmim, o resto contorno neutro; sem dado, so o
/// travessao, sem contorno (vazio nao e controle). Sucesso nao tem cor
/// propria: a paleta do C16 nao tem verde, e OK e o estado que nao pede nada.
pub fn rec_class(code: u32) -> &'static str {
    match code {
        4 | 6 => "rec-error",
        3 => "rec-warning",
        2 => "rec-ok",
        REC_NONE => "rec-none",
        _ => "rec-neutral",
    }
}

/// `human` do contexto.py: `1.2M`, `3.4k`, `950`.
pub fn human(n: u64) -> String {
    let v = n as f64;
    if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

/// O motivo no texto do contexto.py (funcao `analyse`).
pub fn reason_text(r: &Reason) -> String {
    match *r {
        Reason::Ociosa { idle_min } => format!("sem atividade ha {idle_min:.0} min"),
        Reason::Loop { repeats } => format!("chamada repetida {repeats}x"),
        Reason::ContextoUrgente {
            ctx_pct,
            limite_pct,
        }
        | Reason::ContextoAlto {
            ctx_pct,
            limite_pct,
        } => format!("contexto {ctx_pct:.0}% >= {limite_pct}%"),
        Reason::Releitura {
            share,
            limite,
            piso,
            turno,
            payback,
        } => format!(
            "releitura {:.0}% (>= {:.0}%) com contexto acima do piso de {}; cada turno rele {}; \
             sessao nova se paga em {} turno(s)",
            share * 100.0,
            limite * 100.0,
            human(piso),
            human(turno),
            payback.map_or_else(|| "?".to_string(), |p| p.to_string()),
        ),
        Reason::HandoffVelho { age_min } => format!("ultimo handoff ha {:.0}h", age_min / 60.0),
    }
}

/// Idade do handoff: `35m` abaixo de uma hora, `4h` acima.
pub fn age(min: f64) -> String {
    if min < 60.0 {
        format!("{min:.0}m")
    } else {
        format!("{:.0}h", min / 60.0)
    }
}

/// A recomendacao para o leitor de tela: a palavra e, quando pede acao, que
/// precisa de voce. A pilula mostra o nome do script; a fala diz o sentido.
pub fn rec_spoken(code: u32) -> &'static str {
    match code {
        REC_PENDING => "reading the transcript",
        2 => "continue",
        3 => "compact, needs you",
        4 => "handoff, needs you",
        5 => "idle",
        6 => "loop, needs you",
        _ => "no cockpit data",
    }
}

/// Idade do handoff por extenso, com o numero: `35 minutes ago`, `5 hours ago`.
pub fn age_spoken(min: f64) -> String {
    let (n, unit) = if min < 60.0 {
        (min.round(), "minute")
    } else {
        ((min / 60.0).round(), "hour")
    };
    format!("{n:.0} {unit}{} ago", if n == 1.0 { "" } else { "s" })
}

/// O que o cockpit diz da sessao, lido de `overlay.cockpit` e
/// `overlay.cockpit_pending` (C21). Os tipos sao os do modulo `cockpit` do
/// core: a GUI so formata, nao refaz conta.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CockpitView {
    /// Varredura do transcript ainda em curso: pilula "lendo".
    pub pending: bool,
    pub verdict: Option<Recommendation>,
    pub reasons: Vec<Reason>,
    /// Burn ponderado (tokens) e a fatia do orcamento (`Contas`).
    pub burn: Option<u64>,
    pub burn_pct: Option<f64>,
    /// `cockpit::cache_hit`, 0..=1.
    pub cache_hit: Option<f64>,
    pub handoff_age_min: Option<f64>,
    /// Arquivo do ultimo handoff do projeto, ja saneado pelo core.
    pub handoff_name: Option<String>,
}

impl CockpitView {
    /// Sem `overlay.cockpit` a vista fica vazia: so `pending` diz se o
    /// transcript ainda esta sendo lido.
    pub fn of(o: Option<&SessionOverlay>) -> Self {
        let pending = o.is_some_and(|o| o.cockpit_pending);
        let Some(c) = o.and_then(|o| o.cockpit.as_ref()) else {
            return CockpitView {
                pending,
                ..CockpitView::default()
            };
        };
        CockpitView {
            pending,
            verdict: Some(c.recommendation),
            reasons: c.reasons.clone(),
            burn: Some(c.burn_weighted),
            burn_pct: Some(c.burn_pct),
            cache_hit: c.cache_hit,
            handoff_age_min: c.handoff_age_min,
            handoff_name: c.handoff_name.as_deref().map(|n| sanitize(n).into_owned()),
        }
    }
}

/// Uma linha da lista: texto de cada campo e chave numerica das ordenaveis.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub pid: u32,
    pub agent: String,
    pub name: String,
    /// Texto da pilula de estado; vazio no multiplexador.
    pub status: String,
    pub group: u32,
    pub cpu: f64,
    pub cpu_text: String,
    pub cpu_frac: f64,
    pub cpu_tooltip: String,
    pub pss: i64,
    pub pss_text: String,
    /// PSS + SwapPss, a ordem do snapshot.
    pub mem: i64,
    pub mem_text: String,
    pub io_bound: bool,
    pub io_text: String,
    pub io_tooltip: String,
    pub ctx: f64,
    pub ctx_text: String,
    pub ctx_frac: f64,
    /// Pane herdr (`w4:p16`), ou o workspace.
    pub place: String,
    pub cwd: String,
    /// `agente · provider · modelo`.
    pub meta: String,
    pub rec: u32,
    pub rec_text: String,
    pub rec_tooltip: String,
    pub burn: f64,
    pub burn_text: String,
    pub burn_tooltip: String,
    pub cache_text: String,
    pub cache_tooltip: String,
    pub handoff_text: String,
    pub handoff_tooltip: String,
    pub tools_text: String,
    pub tools_tooltip: String,
    pub sub_text: String,
    pub sub_tooltip: String,
    pub a11y_label: String,
}

fn cell<T: Copy>(
    s: &Session,
    m: Metric,
    v: Option<T>,
    key: impl Fn(T) -> f64,
    text: impl Fn(T) -> String,
) -> (f64, String) {
    match v {
        None => (-1.0, DASH.to_string()),
        Some(x) if s.partial.contains(&m) => (key(x), format!("≥{}", text(x))),
        Some(x) => (key(x), text(x)),
    }
}

fn text_or(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| sanitize(t).into_owned())
}

/// CPU em % de um nucleo: uma casa abaixo de 10, inteiro acima.
pub fn cpu_pct(v: f64) -> String {
    if v < 10.0 {
        format!("{v:.1}%")
    } else {
        format!("{v:.0}%")
    }
}

/// Nome do provider para gente: as tres familias pelo nome, o resto pelo id.
pub fn provider_name(id: &str) -> String {
    match cockpit::family(id) {
        Family::Anthropic => "Anthropic".into(),
        Family::OpenAI => "OpenAI".into(),
        Family::Google => "Google".into(),
        Family::Outro => id.to_string(),
    }
}

impl Row {
    pub fn from_session(s: &Session, source: IoStallSource, home: Option<&str>) -> Self {
        let ov = s.overlay.as_ref();
        let herdr = ov.and_then(|o| o.herdr.as_ref());
        let group = Group::of(s);
        let mux = group == Group::Multiplexer;
        let agent = sanitize(&s.agent).into_owned();
        let cwd_full = s.cwd.as_deref().map(|c| short_path(c, home));
        let name = text_or(ov.and_then(|o| o.name.as_deref()))
            .or_else(|| text_or(herdr.and_then(|h| h.title.as_deref())))
            .or_else(|| {
                // O multiplexador se chama pelo agente; o cwd vai na 2a linha.
                let c = s.cwd.as_deref().filter(|_| !mux)?.trim_end_matches('/');
                text_or(c.rsplit('/').next())
            })
            .unwrap_or_else(|| agent.clone());

        let (cpu, cpu_text) = cell(s, Metric::CpuPct, s.cpu_pct, |v| v, cpu_pct);
        let cpu_frac = s.cpu_pct.map_or(0.0, |c| (c / 100.0).clamp(0.0, 1.0));
        let procs = match s.procs {
            1 => "1 process".to_string(),
            n => format!("{n} processes"),
        };
        let cpu_tooltip = match s.cpu_pct {
            Some(_) => format!("CPU {cpu_text} of one core, summed over {procs}"),
            None => format!("No CPU sample yet ({procs})"),
        };

        let (pss, pss_text) = cell(
            s,
            Metric::PssBytes,
            s.pss_bytes,
            |v| v as f64,
            |v| bytes(v as f64),
        );
        let (_, swap_text) = cell(
            s,
            Metric::SwapPssBytes,
            s.swap_pss_bytes,
            |v| v as f64,
            |v| bytes(v as f64),
        );
        let mem = match (s.pss_bytes, s.swap_pss_bytes) {
            (None, None) => -1.0,
            (p, w) => (p.unwrap_or(0) + w.unwrap_or(0)) as f64,
        };
        let mem_text = match s.swap_pss_bytes {
            Some(w) if w > 0 => format!("{pss_text} + {swap_text} swap"),
            _ => pss_text.clone(),
        };

        let stall = match (source, s.stall_pct, s.threads_d) {
            (IoStallSource::Delayacct, Some(p), _) => {
                format!("waiting on disk {p:.0}% of the time")
            }
            (IoStallSource::DState, _, Some(d)) => format!("{d} threads waiting on disk"),
            _ => "no I/O stall reading".to_string(),
        };
        let rate =
            |v: Option<f64>| v.map_or_else(|| DASH.to_string(), |b| format!("{}/s", bytes(b)));
        let io_tooltip = format!(
            "I/O: {stall}; read {}, write {}",
            rate(s.read_bps),
            rate(s.write_bps)
        );

        let (ctx, ctx_text, ctx_frac) = match ov.and_then(|o| o.context_pct) {
            Some(c) => (c, format!("{c:.0}%"), (c / 100.0).clamp(0.0, 1.0)),
            None => (-1.0, DASH.to_string(), 0.0),
        };
        let place = herdr
            .and_then(|h| {
                text_or(h.pane_id.as_deref()).or_else(|| text_or(h.workspace_id.as_deref()))
            })
            .unwrap_or_default();
        let cwd = cwd_full.map_or_else(|| DASH.to_string(), |c| sanitize(&c).into_owned());

        let model = text_or(ov.and_then(|o| o.model.as_deref()));
        // O provider canonico do overlay (declarado > agente > prefixo do
        // modelo, C21); sem overlay de transcript, a mesma funcao aqui.
        let provider = text_or(ov.and_then(|o| o.provider.as_deref()))
            .or_else(|| cockpit::provider(&s.agent, None, model.as_deref()))
            .map(|p| provider_name(&p));
        let meta = match (mux, provider, model) {
            (true, _, _) => format!("{agent} · multiplexer"),
            (false, p, Some(m)) => format!("{agent} · {} · {m}", p.as_deref().unwrap_or(DASH)),
            (false, Some(p), None) => format!("{agent} · {p}"),
            (false, None, None) => agent.clone(),
        };

        let cockpit = CockpitView::of(ov);
        let rec = match cockpit.verdict {
            Some(v) => rec_code(v),
            None if cockpit.pending => REC_PENDING,
            None => REC_NONE,
        };
        let rec_tooltip = match rec {
            REC_NONE => "Cockpit: no data for this session yet".to_string(),
            REC_PENDING => "Cockpit: reading the transcript".to_string(),
            _ if cockpit.reasons.is_empty() => format!("Cockpit: {}", rec_text(rec)),
            _ => cockpit
                .reasons
                .iter()
                .map(reason_text)
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let burn_text = format!(
            "burn {}",
            cockpit.burn.map_or_else(|| DASH.to_string(), human)
        );
        let burn_tooltip = match (cockpit.burn, cockpit.burn_pct) {
            (Some(b), Some(p)) => {
                format!("Weighted burn {}: {p:.1}% of the session budget", human(b))
            }
            (Some(b), None) => format!("Weighted burn {}", human(b)),
            _ => "Weighted burn: no cockpit data yet".to_string(),
        };
        let cache_pct = cockpit.cache_hit.map(|c| format!("{:.0}%", c * 100.0));
        let cache_text = format!("cache {}", cache_pct.as_deref().unwrap_or(DASH));
        let cache_tooltip = match cockpit.cache_hit {
            Some(c) => format!(
                "Prompt cache hit {:.1}%: cache reads over all input tokens",
                c * 100.0
            ),
            None if cockpit.verdict.is_some() => "Prompt cache hit: no input tokens yet".into(),
            None => "Prompt cache hit: no cockpit data yet".into(),
        };
        let handoff_text = format!(
            "handoff {}",
            cockpit
                .handoff_age_min
                .map_or_else(|| DASH.to_string(), age)
        );
        let handoff_spoken = match (cockpit.handoff_age_min, cockpit.verdict) {
            (Some(m), _) => format!("last handoff {}", age_spoken(m)),
            (None, Some(_)) => "no handoff in this project".to_string(),
            (None, None) => "handoff unknown".to_string(),
        };
        let handoff_tooltip = match (&cockpit.handoff_name, cockpit.handoff_age_min) {
            (Some(n), Some(m)) => format!("Last handoff of the project, {}: {n}", age_spoken(m)),
            (None, Some(m)) => format!("Last handoff of the project, {}", age_spoken(m)),
            (_, None) if cockpit.verdict.is_some() => {
                "No handoff found for this project".to_string()
            }
            _ => "Handoff: no cockpit data yet".to_string(),
        };

        let open = ov.and_then(|o| o.tool_calls_open);
        let tools_text = format!(
            "tools {}",
            open.map_or_else(|| DASH.to_string(), |n| n.to_string())
        );
        let tools_tooltip = match (
            open,
            ov.and_then(|o| o.tool_calls),
            ov.and_then(|o| o.tool_calls_per_min),
        ) {
            (None, None, None) => "Tool calls: no transcript source for this agent".to_string(),
            (o, t, m) => format!(
                "Tool calls: {} in flight, {} total, {} per minute",
                o.map_or_else(|| DASH.to_string(), |n| n.to_string()),
                t.map_or_else(|| DASH.to_string(), |n| n.to_string()),
                m.map_or_else(|| DASH.to_string(), |n| format!("{n:.1}")),
            ),
        };
        let live = ov.and_then(|o| o.subagents_live);
        let sub_text = format!(
            "sub {}",
            live.map_or_else(|| DASH.to_string(), |n| n.to_string())
        );
        let sub_tooltip = match (live, ov.and_then(|o| o.subagents_total)) {
            (None, _) => "Subagents: no source yet".to_string(),
            (Some(l), t) => format!(
                "Subagents: {l} live, {} in this session",
                t.map_or_else(|| DASH.to_string(), |n| n.to_string())
            ),
        };

        let status = if mux { "" } else { group.title() };
        // O leitor de tela ouve a linha numa frase: estado, recomendacao com o
        // sentido ("needs you"), e o handoff com o numero, mesmo onde a
        // densidade tirou o bloco do cockpit da tela.
        let a11y_label = if mux {
            format!("{name}, {agent} multiplexer, memory {mem_text}")
        } else if cockpit.verdict.is_some() {
            format!(
                "{name}, {agent}, {status}, recommendation {}, {handoff_spoken}, \
                 context {ctx_text}, {burn_text}, cache hit {}, memory {mem_text}",
                rec_spoken(rec),
                cache_pct.as_deref().unwrap_or("unknown"),
            )
        } else {
            format!(
                "{name}, {agent}, {status}, recommendation {}, context {ctx_text}, memory {mem_text}",
                rec_spoken(rec)
            )
        };
        // Chaves de bytes em i64: cabe ate 8 EiB e o -1 de ausente.
        let key_i64 = |k: f64| if k < 0.0 { -1 } else { k as i64 };
        Row {
            pid: s.pid,
            agent,
            name,
            status: status.to_string(),
            group: group.rank(),
            cpu,
            cpu_text,
            cpu_frac,
            cpu_tooltip,
            pss: key_i64(pss),
            pss_text,
            mem: key_i64(mem),
            mem_text,
            io_bound: s.io_bound,
            io_text: if s.io_bound { "I/O-bound" } else { "" }.to_string(),
            io_tooltip,
            ctx,
            ctx_text,
            ctx_frac,
            place,
            cwd,
            meta,
            rec,
            rec_text: rec_text(rec).to_string(),
            rec_tooltip,
            burn: cockpit.burn.map_or(-1.0, |b| b as f64),
            burn_text,
            burn_tooltip,
            cache_text,
            cache_tooltip,
            handoff_text,
            handoff_tooltip,
            tools_text,
            tools_tooltip,
            sub_text,
            sub_tooltip,
            a11y_label,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::overlay::{HerdrOverlay, OverlayStatus, SessionOverlay};

    fn session() -> Session {
        Session::aggregate("claude".into(), AgentRole::Agent, 42, 16, Vec::new())
    }

    #[test]
    fn bytes_use_iec_units() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(12.0 * 1024.0), "12 KiB");
        assert_eq!(bytes(1.5 * 1024.0), "1.5 KiB");
        assert_eq!(bytes(340.0 * 1024.0 * 1024.0), "340 MiB");
        assert_eq!(bytes(1.25 * 1024.0 * 1024.0 * 1024.0), "1.2 GiB");
        assert_eq!(bytes(-3.0), "0 B");
    }

    #[test]
    fn used_of_shares_the_unit_of_the_total() {
        let gib = 1u64 << 30;
        assert_eq!(used_of(gib / 2, 38 * gib), "0.5 of 38.0 GiB");
        assert_eq!(used_of(700, 900), "700 of 900 B");
    }

    #[test]
    fn absent_values_are_a_dash_and_sort_last() {
        let r = Row::from_session(&session(), IoStallSource::Delayacct, None);
        for t in [
            &r.cpu_text,
            &r.pss_text,
            &r.mem_text,
            &r.ctx_text,
            &r.cwd,
            &r.rec_text,
        ] {
            assert_eq!(t, DASH);
        }
        for (t, prefix) in [
            (&r.burn_text, "burn"),
            (&r.cache_text, "cache"),
            (&r.handoff_text, "handoff"),
            (&r.tools_text, "tools"),
            (&r.sub_text, "sub"),
        ] {
            assert_eq!(*t, format!("{prefix} {DASH}"));
        }
        assert_eq!(
            (r.cpu, r.pss, r.mem, r.ctx, r.burn),
            (-1.0, -1, -1, -1.0, -1.0)
        );
        assert_eq!((r.cpu_frac, r.ctx_frac), (0.0, 0.0));
        assert_eq!((r.rec, r.io_text.as_str()), (REC_NONE, ""));
        assert_eq!(r.name, "claude", "no name, title or cwd: the agent");
        assert_eq!(Group::from_rank(r.group), Group::Unknown);
        assert_eq!(r.status, "Unknown");
    }

    #[test]
    fn zero_is_a_measure_not_absent() {
        let mut s = session();
        s.cpu_pct = Some(0.0);
        s.pss_bytes = Some(0);
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!((r.cpu_text.as_str(), r.cpu), ("0.0%", 0.0));
        assert_eq!((r.pss_text.as_str(), r.pss, r.mem), ("0 B", 0, 0));
    }

    #[test]
    fn partial_sum_is_shown_as_a_floor() {
        let mut s = session();
        s.pss_bytes = Some(3 << 20);
        s.swap_pss_bytes = Some(1 << 20);
        s.partial = vec![Metric::PssBytes];
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!(r.pss_text, "≥3.0 MiB");
        assert_eq!(r.mem_text, "≥3.0 MiB + 1.0 MiB swap");
        assert_eq!((r.pss, r.mem), (3 << 20, 4 << 20));
    }

    #[test]
    fn cpu_bar_is_one_core_capped() {
        let mut s = session();
        s.cpu_pct = Some(250.0);
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!((r.cpu_text.as_str(), r.cpu_frac), ("250%", 1.0));
        s.cpu_pct = Some(4.25);
        assert_eq!(
            Row::from_session(&s, IoStallSource::Delayacct, None).cpu_text,
            "4.2%"
        );
    }

    #[test]
    fn io_tooltip_follows_the_stall_source() {
        let mut s = session();
        s.stall_pct = Some(23.4);
        s.read_bps = Some(2048.0);
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!(
            r.io_tooltip,
            format!("I/O: waiting on disk 23% of the time; read 2.0 KiB/s, write {DASH}")
        );
        s.stall_pct = None;
        s.threads_d = Some(2);
        let r = Row::from_session(&s, IoStallSource::DState, None);
        assert!(
            r.io_tooltip.starts_with("I/O: 2 threads waiting on disk"),
            "{}",
            r.io_tooltip
        );
    }

    #[test]
    fn overlay_fills_name_state_ctx_place_and_meta() {
        let mut s = session();
        s.io_bound = true;
        s.overlay = Some(SessionOverlay {
            name: Some("refactor sampler".into()),
            status: Some(OverlayStatus::Blocked),
            context_pct: Some(64.4),
            model: Some("claude-opus-5-5".into()),
            tool_calls_open: Some(2),
            tool_calls: Some(140),
            herdr: Some(HerdrOverlay {
                workspace_id: Some("w2".into()),
                pane_id: None,
                ..HerdrOverlay::default()
            }),
            ..SessionOverlay::default()
        });
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!(r.name, "refactor sampler");
        assert_eq!(
            (r.status.as_str(), Group::from_rank(r.group)),
            ("Blocked", Group::Blocked)
        );
        assert_eq!(
            (r.ctx_text.as_str(), r.ctx, r.ctx_frac),
            ("64%", 64.4, 0.644)
        );
        assert_eq!(r.place, "w2");
        assert_eq!(r.meta, "claude · Anthropic · claude-opus-5-5");
        assert_eq!(r.io_text, "I/O-bound");
        assert_eq!(r.tools_text, "tools 2");
        assert!(
            r.tools_tooltip.contains("2 in flight, 140 total"),
            "{}",
            r.tools_tooltip
        );
        assert!(
            r.a11y_label.contains("Blocked")
                && r.a11y_label.contains("context 64%")
                && r.a11y_label.contains("recommendation no cockpit data"),
            "{}",
            r.a11y_label
        );
    }

    fn cockpit_rows() -> Vec<Row> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots/cockpit.json");
        let snap: agentcenter_core::Snapshot =
            serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snap");
        snap.sessions
            .iter()
            .map(|s| Row::from_session(s, IoStallSource::Delayacct, None))
            .collect()
    }

    fn by_name<'a>(rows: &'a [Row], name: &str) -> &'a Row {
        rows.iter().find(|r| r.name == name).expect(name)
    }

    #[test]
    fn cockpit_fixture_rows_read_like_the_overlay() {
        // jq '.sessions[] | {n: .overlay.name, p: .overlay.provider,
        //     c: .overlay.cockpit | {burn_weighted, cache_hit, handoff_age_min,
        //     recommendation}}' cockpit.json
        let rows = cockpit_rows();
        let cells = |r: &Row| {
            (
                r.rec_text.clone(),
                r.burn_text.clone(),
                r.cache_text.clone(),
                r.handoff_text.clone(),
            )
        };
        let want = |a: &str, b: &str, c: &str, d: &str| {
            (a.to_string(), b.to_string(), c.to_string(), d.to_string())
        };
        let atlas = by_name(&rows, "atlas-api-auth-refactor");
        assert_eq!(
            cells(atlas),
            want("OK", "burn 1.1M", "cache 92%", "handoff 46m")
        );
        assert_eq!(rec_class(atlas.rec), "rec-ok", "continuar is neutral");
        let ledger = by_name(&rows, "ledger-migration-v4");
        assert_eq!(
            cells(ledger),
            want("COMPACTAR", "burn 2.7M", "cache 90%", "handoff 5h")
        );
        assert_eq!(rec_class(ledger.rec), "rec-warning");
        assert_eq!(
            ledger.rec_tooltip,
            "contexto 62% >= 50%\nultimo handoff ha 5h"
        );
        let orbit = by_name(&rows, "orbit-web-perf-pass");
        assert_eq!(
            cells(orbit),
            want(
                "HANDOFF",
                "burn 8.6M",
                "cache 100%",
                &format!("handoff {DASH}")
            )
        );
        assert_eq!(rec_class(orbit.rec), "rec-error");
        assert_eq!(orbit.meta, "codex · OpenAI · gpt-6-codex");
        assert_eq!(orbit.burn, 8_600_000.0);
        let harbor = by_name(&rows, "harbor-docs");
        assert_eq!(rec_text(harbor.rec), "OCIOSA");
        assert!(harbor.meta.starts_with("pi · deepinfra"), "{}", harbor.meta);
        assert_eq!(rec_text(by_name(&rows, "quill-flaky-tests").rec), "LOOP");
        let relay = by_name(&rows, "relay-i18n");
        assert_eq!(
            (relay.rec, relay.burn_text.as_str()),
            (REC_PENDING, "burn \u{2014}")
        );
        assert_eq!(by_name(&rows, "tundra-onboarding").rec, REC_NONE);
    }

    #[test]
    fn handoff_is_spoken_with_number_and_state() {
        let rows = cockpit_rows();
        let orbit = by_name(&rows, "orbit-web-perf-pass");
        assert!(
            orbit
                .a11y_label
                .contains("recommendation handoff, needs you, no handoff in this project"),
            "{}",
            orbit.a11y_label
        );
        let ledger = by_name(&rows, "ledger-migration-v4");
        assert!(
            ledger
                .a11y_label
                .contains("recommendation compact, needs you, last handoff 5 hours ago"),
            "{}",
            ledger.a11y_label
        );
        assert!(ledger.a11y_label.contains("burn 2.7M, cache hit 90%"));
        let atlas = by_name(&rows, "atlas-api-auth-refactor");
        assert!(
            atlas
                .a11y_label
                .contains("recommendation continue, last handoff 46 minutes ago"),
            "{}",
            atlas.a11y_label
        );
        assert_eq!(
            atlas.handoff_tooltip,
            "Last handoff of the project, 46 minutes ago: \
             20261015-101500-atlas-api-auth-refactor.md"
        );
        assert_eq!(age_spoken(60.0), "1 hour ago");
        assert_eq!(age_spoken(1.0), "1 minute ago");
    }

    #[test]
    fn name_falls_back_to_herdr_title_then_cwd() {
        let mut s = session();
        s.cwd = Some("/home/dev/src/kiln/".into());
        assert_eq!(
            Row::from_session(&s, IoStallSource::Delayacct, None).name,
            "kiln"
        );
        s.overlay = Some(SessionOverlay {
            herdr: Some(HerdrOverlay {
                title: Some("  kiln auth  ".into()),
                status: Some("working".into()),
                ..HerdrOverlay::default()
            }),
            ..SessionOverlay::default()
        });
        let r = Row::from_session(&s, IoStallSource::Delayacct, Some("/home/dev"));
        assert_eq!(r.name, "kiln auth");
        assert_eq!(
            Group::from_rank(r.group),
            Group::Busy,
            "herdr status counts"
        );
        assert_eq!(r.cwd, "~/src/kiln/");
    }

    #[test]
    fn multiplexer_is_its_own_group_without_state() {
        let s = Session::aggregate("herdr".into(), AgentRole::Multiplexer, 7, 16, Vec::new());
        let r = Row::from_session(&s, IoStallSource::Delayacct, None);
        assert_eq!(Group::from_rank(r.group), Group::Multiplexer);
        assert_eq!(
            (r.status.as_str(), r.meta.as_str()),
            ("", "herdr · multiplexer")
        );
        assert!(!r.a11y_label.contains("recommendation"));
        assert_eq!(r.name, "herdr");
    }

    #[test]
    fn cockpit_texts_follow_the_script() {
        assert_eq!(rec_text(rec_code(Recommendation::Continuar)), "OK");
        assert_eq!(rec_code(Recommendation::Handoff), REC_HANDOFF);
        assert_eq!(rec_text(REC_PENDING), "lendo");
        assert_eq!(rec_text(REC_NONE), DASH);
        assert_eq!(rec_class(rec_code(Recommendation::Loop)), "rec-error");
        assert_eq!(
            rec_class(rec_code(Recommendation::Compactar)),
            "rec-warning"
        );
        assert_eq!(
            (human(950), human(1_250), human(3_400_000)),
            ("950".into(), "1.2k".into(), "3.4M".into())
        );
        assert_eq!(
            (age(35.0), age(240.0)),
            ("35m".to_string(), "4h".to_string())
        );
        assert_eq!(
            reason_text(&Reason::ContextoUrgente {
                ctx_pct: 91.6,
                limite_pct: 90
            }),
            "contexto 92% >= 90%"
        );
        assert_eq!(
            reason_text(&Reason::Releitura {
                share: 0.62,
                limite: 0.6,
                piso: 450_000,
                turno: 45_000,
                payback: Some(3.0)
            }),
            "releitura 62% (>= 60%) com contexto acima do piso de 450.0k; cada turno rele 45.0k; \
             sessao nova se paga em 3 turno(s)"
        );
        assert_eq!(
            reason_text(&Reason::HandoffVelho { age_min: 300.0 }),
            "ultimo handoff ha 5h"
        );
        assert_eq!(provider_name("openrouter"), "openrouter");
    }

    #[test]
    fn cwd_under_home_is_abbreviated_and_sanitized() {
        let mut s = session();
        s.cwd = Some("/home/dev/proj\u{202e}cod".into());
        let r = Row::from_session(&s, IoStallSource::Delayacct, Some("/home/dev/"));
        assert_eq!(r.cwd, "~/proj\\u{202e}cod");
        assert_eq!(short_path("/home/devil", Some("/home/dev")), "/home/devil");
        assert_eq!(short_path("/home/dev", Some("/home/dev")), "~");
        assert_eq!(short_path("/x", Some("")), "/x");
    }
}
