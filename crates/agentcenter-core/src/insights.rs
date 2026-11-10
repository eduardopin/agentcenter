//! Motor de achados: regras deterministicas sobre o [`Snapshot`], porte fiel de
//! `findings()`, `facts()` e `fallback()` do prototipo `insights.py`.
//!
//! Puro: sem relogio, sem I/O, sem sorteio. Campo ausente no snapshot vale o
//! mesmo que o `or 0` do prototipo (0, ou "sem dado").

use crate::adapters::AgentRole;
use crate::overlay::OverlayStatus;
use crate::session::Session;
use crate::snapshot::Snapshot;
use serde::Serialize;

const GB: f64 = 1e9;
/// Maximo de achados devolvidos.
const MAX_FINDINGS: usize = 4;

/// Um achado: o texto de regra e as strings que um resumo fiel do primeiro
/// achado precisa manter (`must`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub id: String,
    pub severity: String,
    pub score: u32,
    pub text: String,
    pub must: Vec<String>,
}

fn finding(id: &str, severity: &str, score: u32, text: String, must: Vec<String>) -> Finding {
    Finding {
        id: id.into(),
        severity: severity.into(),
        score,
        text,
        must,
    }
}

/// `basename(cwd)` ou, se vazio, o nome do agente; mais o pid. O nome so leva
/// `[A-Za-z0-9._-]` (o resto vira `_`, no maximo 40): ele entra no texto dos achados e no
/// prompt do modelo, e o cwd e dado de fora.
fn label(s: &Session) -> String {
    let base = s
        .cwd
        .as_deref()
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("");
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let name = if safe.chars().any(|c| c.is_ascii_alphanumeric()) {
        safe.as_str()
    } else {
        s.agent.as_str()
    };
    format!("{name} (pid {})", s.pid)
}

fn gb(n: f64) -> String {
    format!("{:.1} GB", n / GB)
}

/// So o numero de `gb`, sem a unidade.
fn gb_num(n: f64) -> String {
    gb(n).trim_end_matches(" GB").to_string()
}

/// `round()` do Python arredonda metade para o par.
fn round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

fn plural<'a>(n: usize, many: &'a str, one: &'a str) -> &'a str {
    if n > 1 { many } else { one }
}

fn pss(s: &Session) -> f64 {
    s.pss_bytes.unwrap_or(0) as f64
}

fn ctx_pct(s: &Session) -> f64 {
    s.overlay
        .as_ref()
        .and_then(|o| o.context_pct)
        .unwrap_or(0.0)
}

fn streak(s: &Session) -> u32 {
    cockpit(s).map_or(0, |c| c.max_streak)
}

fn cockpit(s: &Session) -> Option<&crate::cockpit::Cockpit> {
    s.overlay.as_ref().and_then(|o| o.cockpit.as_ref())
}

fn idle_min(s: &Session) -> f64 {
    cockpit(s).and_then(|c| c.idle_min).unwrap_or(0.0)
}

fn burn(s: &Session) -> u64 {
    cockpit(s).map_or(0, |c| c.burn_weighted)
}

/// `cache_hit or 1` do prototipo: ausente e 0.0 valem 1 (sem dado).
fn cache_hit(s: &Session) -> f64 {
    match cockpit(s).and_then(|c| c.cache_hit) {
        Some(h) if h != 0.0 => h,
        _ => 1.0,
    }
}

/// Primeiro maximo por `key`, como o `max()` do Python (empate fica com o primeiro).
fn first_max<'a>(v: &[&'a Session], key: impl Fn(&Session) -> f64) -> &'a Session {
    let mut best = v[0];
    for s in &v[1..] {
        if key(s) > key(best) {
            best = s;
        }
    }
    best
}

/// Idem para o minimo.
fn first_min<'a>(v: &[&'a Session], key: impl Fn(&Session) -> f64) -> &'a Session {
    let mut best = v[0];
    for s in &v[1..] {
        if key(s) < key(best) {
            best = s;
        }
    }
    best
}

/// Ate 4 achados, do mais urgente; `all_clear` quando nenhuma regra dispara.
pub fn findings(snap: &Snapshot) -> Vec<Finding> {
    let m = &snap.machine;
    let mut out: Vec<Finding> = Vec::new();
    let agents: Vec<&Session> = snap
        .sessions
        .iter()
        .filter(|s| s.role == AgentRole::Agent)
        .collect();

    let loops: Vec<&Session> = agents.iter().copied().filter(|s| streak(s) >= 5).collect();
    if !loops.is_empty() {
        let w = first_max(&loops, |s| f64::from(streak(s)));
        let n = streak(w);
        out.push(finding(
            "loop",
            "high",
            100,
            format!(
                "{} session{} repeating the same tool call; {} repeated it {n} times in a row. \
                 Action: interrupt that session and redirect it.",
                loops.len(),
                plural(loops.len(), "s are", " is"),
                label(w)
            ),
            vec![n.to_string(), w.pid.to_string()],
        ));
    }

    let avail = m.mem_available_bytes.unwrap_or(0) as f64;
    let total = m.mem_total_bytes.unwrap_or(0) as f64;
    let swap_used = m
        .swap_total_bytes
        .unwrap_or(0)
        .saturating_sub(m.swap_free_bytes.unwrap_or(0)) as f64;
    let sess_mem: f64 = agents.iter().map(|s| pss(s)).sum();
    let idle: Vec<&Session> = agents
        .iter()
        .copied()
        .filter(|s| {
            s.overlay.as_ref().and_then(|o| o.status) == Some(OverlayStatus::Idle)
                && idle_min(s) >= 30.0
        })
        .collect();
    let idle_mem: f64 = idle.iter().map(|s| pss(s)).sum();
    // Sem total de memoria nao ha fracao a avaliar.
    if total > 0.0 && avail / total < 0.15 {
        let pct = round(100.0 * avail / total);
        out.push(finding(
            "memory_pressure",
            "high",
            90,
            format!(
                "Only {} of {} memory is available ({pct}%), with {} in swap; agent sessions hold {}. \
                 Action: agentcenter-cli reclaim.",
                gb(avail),
                gb(total),
                gb(swap_used),
                gb(sess_mem)
            ),
            vec![gb_num(avail), "reclaim".into()],
        ));
    }

    let full: Vec<&Session> = agents
        .iter()
        .copied()
        .filter(|s| ctx_pct(s) >= 75.0)
        .collect();
    if !full.is_empty() {
        let w = first_max(&full, ctx_pct);
        let p = round(ctx_pct(w));
        out.push(finding(
            "context_full",
            "high",
            80,
            format!(
                "{} session{} past 75% of the context window; the fullest is {} at {p}%. \
                 Action: write a handoff and start a fresh session.",
                full.len(),
                plural(full.len(), "s are", " is"),
                label(w)
            ),
            vec![format!("{p}%"), "handoff".into()],
        ));
    }

    if !idle.is_empty() && (idle_mem >= 2.0 * GB || idle_mem / sess_mem.max(1.0) >= 0.25) {
        let w = first_max(&idle, pss);
        out.push(finding(
            "idle_memory",
            "medium",
            60,
            format!(
                "{} sessions idle for 30 minutes or more hold {} of the {} used by agents; \
                 the largest is {} with {}. Action: agentcenter-cli reclaim.",
                idle.len(),
                gb(idle_mem),
                gb(sess_mem),
                label(w),
                gb(pss(w))
            ),
            vec![gb_num(idle_mem), "reclaim".into()],
        ));
    }

    if let Some(la) = &m.load_avg
        && la.one > f64::from(m.cores)
    {
        out.push(finding(
            "cpu_saturated",
            "medium",
            55,
            format!(
                "Load average is {:.1} on {} cores. \
                 Action: pause the busiest session with agentcenter-cli signal.",
                la.one, m.cores
            ),
            vec![format!("{:.1}", la.one), m.cores.to_string()],
        ));
    }

    let tot: u64 = agents.iter().map(|s| burn(s)).sum();
    if tot > 0 {
        let w = first_max(&agents, |s| burn(s) as f64);
        let share = round(100.0 * burn(w) as f64 / tot as f64);
        if share >= 40 && agents.len() >= 3 {
            out.push(finding(
                "burn_leader",
                "medium",
                40,
                format!(
                    "{} accounts for {share}% of the token burn of all {} sessions. \
                     Action: check whether it still needs its full context.",
                    label(w),
                    agents.len()
                ),
                vec![format!("{share}%"), w.pid.to_string()],
            ));
        }
    }

    let cold: Vec<&Session> = agents
        .iter()
        .copied()
        .filter(|s| {
            s.overlay.as_ref().and_then(|o| o.tokens_in).unwrap_or(0) >= 1_000_000
                && cache_hit(s) < 0.8
        })
        .collect();
    if !cold.is_empty() {
        let w = first_min(&cold, cache_hit);
        let hit = round(100.0 * cache_hit(w));
        out.push(finding(
            "cache_cold",
            "low",
            30,
            format!(
                "{} large session{} a prompt-cache hit rate under 80%; the lowest is {} at {hit}%. \
                 Action: avoid long pauses between turns in that session.",
                cold.len(),
                plural(cold.len(), "s have", " has"),
                label(w)
            ),
            vec![format!("{hit}%"), w.pid.to_string()],
        ));
    }

    // Estavel: empate de score mantem a ordem das regras.
    out.sort_by_key(|f| std::cmp::Reverse(f.score));
    if out.is_empty() {
        out.push(finding(
            "all_clear",
            "info",
            0,
            format!(
                "{} sessions are running with {} of memory available and no session needing attention. \
                 Action: none.",
                agents.len(),
                gb(avail)
            ),
            vec![agents.len().to_string(), gb_num(avail)],
        ));
    }
    out.truncate(MAX_FINDINGS);
    out
}

/// Os achados numerados, um por linha, para o prompt do modelo.
pub fn facts(fs: &[Finding]) -> String {
    fs.iter()
        .enumerate()
        .map(|(i, f)| format!("{}. [{}] {}", i + 1, f.severity, f.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// O texto de regra mostrado sem modelo: o primeiro achado e quantos mais ha.
/// `fs` nao pode ser vazio (`findings` nunca devolve vazio); vazio da texto vazio.
pub fn fallback(fs: &[Finding]) -> String {
    let Some(first) = fs.first() else {
        return String::new();
    };
    let more = fs.len() - 1;
    if more == 0 {
        first.text.clone()
    } else {
        format!(
            "{} {more} more finding{} below.",
            first.text,
            if more > 1 { "s" } else { "" }
        )
    }
}
