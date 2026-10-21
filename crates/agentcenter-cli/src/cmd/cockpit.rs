//! `agentcenter-cli cockpit [--json] [--all] [--timeout-ms N] [--proc-root PATH]`
//!
//! A visao do `contexto.py` do squad sobre as sessoes vivas: uma linha por
//! sessao de agente com a recomendacao do cockpit (C21) e, abaixo, os motivos
//! no texto do script. As contas sao do core (`overlay.cockpit`); aqui so a
//! amostragem ate o cockpit sair e a formatacao, com os mesmos arredondamentos
//! do script (`human`, `:.0%`, `:g`).

use agentcenter_core::adapters::AgentRole;
use agentcenter_core::cockpit::{Reason, Recommendation, ctx_pct, round_n};
use agentcenter_core::{ProcRoot, Registry, Sampler, SamplerConfig, Session, Snapshot, Summary};
use std::fmt::Write as _;
use std::io::Write;
use std::time::{Duration, Instant};

const HELP: &str = "\
usage: agentcenter-cli cockpit [--json] [--all] [--timeout-ms N] [--proc-root PATH]

One row per live agent session with the cockpit of contexto.py: what to do
(REC: CONTINUAR, COMPACTAR, HANDOFF, OCIOSA), context, weighted burn, cache
hit, re-read share and payback, and below it the reasons (`motivo:`).
Transcripts are read in full; sampling repeats until every matched
transcript is read or the timeout runs out. On timeout what is ready is
printed, stderr says how many sessions are still reading, and the exit code
is 0.

  --json            print the snapshot as JSON (same as `snapshot --json`)
  --all             also list idle sessions (OCIOSA), hidden by default
  --timeout-ms N    stop sampling after N ms (default 15000)
  --proc-root PATH  read PATH instead of /proc (fixtures)

Columns: CTX is context tokens / window and % (`~`: window inferred), BURN
the weighted burn and % of the session budget (CLAUDE_SESSION_BUDGET), CACHE
cache_read / tokens_in, RELEI the weighted re-read share (it decides
HANDOFF), PBACK turns until a new session pays off, HANDOFF the age and name
of the project's last handoff, IDLE minutes since the transcript was written.

Not measured here (contexto.py does): LOOP (repeated tool calls) and files
re-read many times. A session the script calls LOOP shows its other verdict.

The thresholds are the script's: SQUAD_CTX_PCT, SQUAD_REREAD,
SQUAD_REREAD_PISO, SQUAD_BURN_CHEIO, SQUAD_CTX_BASE, CLAUDE_SESSION_BUDGET.
env AGENTCENTER_COCKPIT_READ_BUDGET=N caps transcript bytes per tick (tests).";

/// Orcamento de leitura por tick, so para teste (forcar o timeout).
pub const READ_BUDGET_ENV: &str = "AGENTCENTER_COCKPIT_READ_BUDGET";

/// Entre os dois primeiros ticks, como o `snapshot` (o herdr responde nesse tempo).
const FIRST_GAP: Duration = Duration::from_millis(1000);
/// Entre os ticks seguintes, enquanto houver transcript lendo.
const RETRY_GAP: Duration = Duration::from_millis(250);
/// Caracteres do nome do handoff na coluna HANDOFF.
const HANDOFF_NAME_COLS: usize = 24;

const HEADERS: [&str; 13] = [
    "REC", "AGENT", "PROVIDER", "MODEL", "NAME", "CTX", "BURN", "CACHE", "RELEI", "PBACK",
    "HANDOFF", "IDLE", "CWD",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub all: bool,
    pub timeout_ms: u64,
    pub proc_root: Option<String>,
}

/// `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        json: false,
        all: false,
        timeout_ms: 15_000,
        proc_root: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--json" => o.json = true,
            "--all" => o.all = true,
            "--timeout-ms" => {
                let v = value("--timeout-ms")?;
                o.timeout_ms = v
                    .parse()
                    .map_err(|_| format!("--timeout-ms: `{v}` is not a number of milliseconds"))?;
            }
            "--proc-root" => o.proc_root = Some(value("--proc-root")?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("cockpit: unknown option `{other}`\n\n{HELP}")),
        }
    }
    Ok(Some(o))
}

/// A do `snapshot` (transcripts inteiros), com o teto de teste se houver.
fn config(budget: Option<&str>) -> SamplerConfig {
    let mut c = super::snapshot::one_shot_config();
    if let Some(b) = budget.and_then(|b| b.trim().parse::<u64>().ok()) {
        c.transcript_read_budget = b;
    }
    c
}

fn nap(d: Duration, deadline: Instant) {
    std::thread::sleep(d.min(deadline.saturating_duration_since(Instant::now())));
}

/// Dois ticks, como o `snapshot`, e mais ate nenhum transcript casado estar
/// lendo (`summary.cockpit_pending == 0`) ou o prazo acabar.
fn sample(root: &ProcRoot, config: SamplerConfig, timeout: Duration) -> Result<Snapshot, String> {
    let deadline = Instant::now() + timeout;
    let mut sampler = Sampler::with_config(Registry::from_env(), config);
    let err = |e| format!("reading {}: {e}", root.path().display());
    sampler.tick(root).map_err(err)?;
    nap(FIRST_GAP, deadline);
    let mut snap = sampler.tick(root).map_err(err)?;
    while snap.summary.cockpit_pending > 0 && Instant::now() < deadline {
        nap(RETRY_GAP, deadline);
        snap = sampler.tick(root).map_err(err)?;
    }
    Ok(snap)
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(opts) = parse(&args)? else {
        println!("{HELP}");
        return Ok(());
    };
    let root = opts
        .proc_root
        .as_deref()
        .map_or_else(ProcRoot::default, ProcRoot::new);
    let budget = std::env::var(READ_BUDGET_ENV).ok();
    let snap = sample(
        &root,
        config(budget.as_deref()),
        Duration::from_millis(opts.timeout_ms),
    )?;

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let res = if opts.json {
        serde_json::to_writer(&mut out, &snap)
            .map_err(|e| e.to_string())
            .and_then(|()| writeln!(out).map_err(|e| e.to_string()))
    } else {
        let home = std::env::var("HOME").ok();
        out.write_all(render(&snap, opts.all, home.as_deref()).as_bytes())
            .map_err(|e| e.to_string())
    };
    // Pipe fechado (`| head`) nao e erro.
    let res = match res.and_then(|()| out.flush().map_err(|e| e.to_string())) {
        Err(e) if e.contains("Broken pipe") => Ok(()),
        r => r,
    };
    let pending = snap.summary.cockpit_pending;
    if pending > 0 {
        eprintln!(
            "agentcenter-cli cockpit: {pending} {} ainda lendo o transcript apos {} ms",
            if pending == 1 { "sessao" } else { "sessoes" },
            opts.timeout_ms
        );
    }
    res
}

/// `human()` do script: unidades decimais, `M` e `k` com uma casa.
pub fn human(n: f64) -> String {
    for (unit, div) in [("M", 1e6), ("k", 1e3)] {
        if n >= div {
            return format!("{:.1}{unit}", n / div);
        }
    }
    format!("{n:.0}")
}

/// `format(x, '.0%')` do Python (multiplica por 100 em ponto flutuante).
fn pct0(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

/// `format(x, '.0f')` sem o `-0` que o Rust escreve e o `round()` do script nao.
fn int0(x: f64) -> String {
    let s = format!("{x:.0}");
    if s == "-0" { "0".to_string() } else { s }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// `format(x, 'g')` do Python: 6 algarismos significativos, notacao
/// cientifica fora de [1e-4, 1e6), zeros finais cortados.
pub fn py_g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp) as usize;
        trim_zeros(&format!("{x:.decimals$}")).to_string()
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_zeros(mant), exp.abs())
    }
}

/// O motivo no texto de `analyse()` do script.
pub fn reason_text(r: &Reason) -> String {
    match *r {
        Reason::Ociosa { idle_min } => format!("sem atividade ha {} min", int0(idle_min)),
        Reason::Loop { repeats } => format!("chamada repetida {repeats}x"),
        Reason::ContextoUrgente {
            ctx_pct,
            limite_pct,
        }
        | Reason::ContextoAlto {
            ctx_pct,
            limite_pct,
        } => format!("contexto {}% >= {limite_pct}%", int0(ctx_pct)),
        Reason::Releitura {
            share,
            limite,
            piso,
            turno,
            payback,
        } => format!(
            "releitura {} (>= {}) com contexto acima do piso de {}; cada turno rele {}; \
             sessao nova se paga em {} turno(s)",
            pct0(share),
            pct0(limite),
            human(piso as f64),
            human(turno as f64),
            payback.map_or_else(|| "?".to_string(), py_g),
        ),
        Reason::HandoffVelho { age_min } => {
            format!("ultimo handoff ha {}h", int0(age_min / 60.0))
        }
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Primeira linha, so de `summary` (todas as sessoes de agente, ociosas
/// inclusive): `3 handoff, 2 compactar, 20 continuar, 4 ociosas | burn 41.2M |
/// cache 82%`. LOOP e sem dado so aparecem quando ha.
pub fn summary_line(s: &Summary) -> String {
    let c = &s.cockpit;
    let mut line = format!(
        "{} handoff, {} compactar, {} continuar, {}",
        c.handoff,
        c.compactar,
        c.continuar,
        count(c.ociosa, "ociosa", "ociosas"),
    );
    if c.loop_ > 0 {
        let _ = write!(line, ", {} loop", c.loop_);
    }
    if c.sem_dado > 0 {
        let _ = write!(line, ", {} sem dado", c.sem_dado);
        if s.cockpit_pending > 0 {
            let _ = write!(line, " ({} lendo)", s.cockpit_pending);
        }
    }
    let _ = write!(
        line,
        " | burn {} | cache {}",
        s.sessions_burn_weighted
            .map_or_else(|| "-".to_string(), |b| human(b as f64)),
        s.cache_hit.map_or_else(|| "-".to_string(), pct0),
    );
    line
}

fn rec_name(r: Recommendation) -> &'static str {
    match r {
        Recommendation::Continuar => "CONTINUAR",
        Recommendation::Compactar => "COMPACTAR",
        Recommendation::Handoff => "HANDOFF",
        Recommendation::Ociosa => "OCIOSA",
        Recommendation::Loop => "LOOP",
    }
}

/// `c` e `h` ou fica dentro de `h`; `h` vazio nunca abrevia (como a tabela).
fn under_home(c: &str, h: &str) -> bool {
    !h.is_empty()
        && c.strip_prefix(h)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn cwd_cell(cwd: Option<&str>, home: Option<&str>) -> String {
    match (cwd, home.map(|h| h.trim_end_matches('/'))) {
        (Some(c), Some(h)) if under_home(c, h) => format!("~{}", &c[h.len()..]),
        (Some(c), _) => c.to_string(),
        (None, _) => "-".to_string(),
    }
}

fn age_cell(min: f64) -> String {
    if min < 60.0 {
        format!("{}m", int0(min))
    } else {
        format!("{}h", int0(min / 60.0))
    }
}

fn dash(v: Option<String>) -> String {
    v.unwrap_or_else(|| "-".to_string())
}

/// Celulas da sessao e a linha de motivos (vazia sem motivo).
fn row(s: &Session, home: Option<&str>) -> ([String; 13], String) {
    let o = s.overlay.as_ref();
    let c = o.and_then(|o| o.cockpit.as_ref());
    let name = o.and_then(|o| {
        o.name
            .clone()
            .or_else(|| o.herdr.as_ref().and_then(|h| h.title.clone()))
    });
    let rec = match c {
        Some(c) => rec_name(c.recommendation).to_string(),
        None if o.is_some_and(|o| o.cockpit_pending) => "lendo".to_string(),
        None => "-".to_string(),
    };
    let ctx = c.map(|c| {
        let tilde = if c.window_inferred { "~" } else { "" };
        if c.window == 0 {
            format!("{}/?", human(c.ctx_tokens as f64))
        } else {
            format!(
                "{}/{}{tilde} {}%",
                human(c.ctx_tokens as f64),
                human(c.window as f64),
                int0(ctx_pct(c.ctx_tokens, c.window)),
            )
        }
    });
    let handoff = c.and_then(|c| {
        let age = age_cell(c.handoff_age_min?);
        Some(match c.handoff_name.as_deref() {
            Some(n) if n.chars().count() > HANDOFF_NAME_COLS => {
                let cut: String = n.chars().take(HANDOFF_NAME_COLS - 1).collect();
                format!("{age} {cut}…")
            }
            Some(n) => format!("{age} {n}"),
            None => age,
        })
    });
    let cells = [
        rec,
        s.agent.clone(),
        dash(o.and_then(|o| o.provider.clone())),
        dash(o.and_then(|o| o.model.clone())),
        dash(name),
        dash(ctx),
        dash(c.map(|c| format!("{} {:.1}%", human(c.burn_weighted as f64), c.burn_pct))),
        dash(c.and_then(|c| c.cache_hit).map(pct0)),
        dash(c.map(|c| pct0(c.reread_share))),
        dash(
            c.and_then(|c| c.payback_turns)
                .filter(|p| *p != 0.0)
                .map(|p| py_g(p) + "t"),
        ),
        dash(handoff),
        dash(c.and_then(|c| c.idle_min).map(|m| int0(m) + "m")),
        cwd_cell(s.cwd.as_deref(), home),
    ];
    let why: Vec<String> = c
        .map(|c| c.reasons.iter().map(reason_text).collect())
        .unwrap_or_default();
    (cells, why.join("; "))
}

/// `(-ctx_pct, -burn)` do script, com o `ctx_pct` arredondado como ele o
/// publica; sessao sem cockpit vai para o fim, na ordem do snapshot.
fn sort_key(s: &Session) -> Option<(f64, u64)> {
    let c = s.overlay.as_ref()?.cockpit.as_ref()?;
    Some((round_n(ctx_pct(c.ctx_tokens, c.window), 1), c.burn_weighted))
}

fn listed(s: &Session, all: bool) -> bool {
    s.role == AgentRole::Agent
        && (all
            || s.overlay
                .as_ref()
                .and_then(|o| o.cockpit.as_ref())
                .is_none_or(|c| c.recommendation != Recommendation::Ociosa))
}

/// Resumo, cabecalho e uma linha por sessao de agente (mais a de motivos).
/// Toda celula passa por [`crate::table::sanitize`]: nome, cwd, modelo,
/// provider e handoff vem de processo, herdr ou transcript.
pub fn render(snap: &Snapshot, all: bool, home: Option<&str>) -> String {
    let mut sessions: Vec<&Session> = snap.sessions.iter().filter(|s| listed(s, all)).collect();
    sessions.sort_by(|a, b| match (sort_key(a), sort_key(b)) {
        (Some((pa, ba)), Some((pb, bb))) => pb.total_cmp(&pa).then(bb.cmp(&ba)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let clean = |c: String| crate::table::sanitize(&c).into_owned();
    let mut rows: Vec<([String; 13], String)> = vec![(HEADERS.map(String::from), String::new())];
    rows.extend(sessions.iter().map(|s| {
        let (cells, why) = row(s, home);
        (cells.map(clean), clean(why))
    }));

    let mut out = clean(summary_line(&snap.summary));
    out.push('\n');
    if rows.len() == 1 {
        out.push_str("nenhuma sessao no recorte\n");
        return out;
    }
    let mut widths = [0usize; 13];
    for (r, _) in &rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    for (r, why) in &rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            // Numeros a direita (CTX..IDLE), texto a esquerda; CWD sem preenchimento.
            if i == 12 {
                line.push_str(c);
            } else if (5..=11).contains(&i) && i != 10 {
                let _ = write!(line, "{c:>w$}  ", w = widths[i]);
            } else {
                let _ = write!(line, "{c:<w$}  ", w = widths[i]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
        if !why.is_empty() {
            let _ = writeln!(out, "{:9} motivo: {why}", "");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::cockpit::{Cockpit, Thresholds, Tokens};
    use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
    use agentcenter_core::overlay::{HerdrOverlay, SessionOverlay};
    use agentcenter_core::{CockpitCounts, READ_ALL};

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_defaults_and_flags() {
        let o = parse(&args(&[])).unwrap().unwrap();
        assert_eq!(
            o,
            Options {
                json: false,
                all: false,
                timeout_ms: 15_000,
                proc_root: None
            }
        );
        let o = parse(&args(&[
            "--json",
            "--all",
            "--timeout-ms",
            "250",
            "--proc-root=/x",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            o,
            Options {
                json: true,
                all: true,
                timeout_ms: 250,
                proc_root: Some("/x".into())
            }
        );
        assert!(parse(&args(&["--timeout-ms", "abc"])).is_err());
        assert!(parse(&args(&["--timeout-ms"])).is_err());
        assert!(parse(&args(&["--interval-ms", "5"])).is_err());
        assert_eq!(parse(&args(&["-h"])).unwrap(), None);
    }

    #[test]
    fn config_reads_in_full_unless_the_test_budget_is_set() {
        assert_eq!(config(None).transcript_read_budget, READ_ALL);
        assert_eq!(config(Some("x")).transcript_read_budget, READ_ALL);
        assert_eq!(config(Some(" 1 ")).transcript_read_budget, 1);
        assert_eq!(
            SamplerConfig {
                transcript_read_budget: READ_ALL,
                ..config(Some("1"))
            },
            super::super::snapshot::one_shot_config()
        );
    }

    /// Valores de referencia gerados com `contexto.human`, `format(x, 'g')` e
    /// `format(x, '.0%')` do proprio script.
    #[test]
    fn number_formats_match_the_script() {
        for (n, want) in [
            (0.0, "0"),
            (999.0, "999"),
            (999.5, "1000"),
            (1000.0, "1.0k"),
            (92_811.0, "92.8k"),
            (999_999.0, "1000.0k"),
            (1_000_000.0, "1.0M"),
            (162_073.0, "162.1k"),
            (41_234_567.0, "41.2M"),
        ] {
            assert_eq!(human(n), want, "{n}");
        }
        for (x, want) in [
            (7.3, "7.3"),
            (240.0, "240"),
            (6_350_000.0, "6.35e+06"),
            (0.5, "0.5"),
            (123_456.7, "123457"),
            (1e-5, "1e-05"),
            (600_000.0, "600000"),
            (999_999.5, "1e+06"),
        ] {
            assert_eq!(py_g(x), want, "{x}");
        }
        for (x, want) in [
            (0.165, "16%"),
            (0.125, "12%"),
            (0.751, "75%"),
            (0.465, "46%"),
        ] {
            assert_eq!(pct0(x), want, "{x}");
        }
        assert_eq!(int0(-0.2), "0");
        assert_eq!(int0(2.5), "2");
    }

    #[test]
    fn reasons_read_like_the_script() {
        let cases = [
            (Reason::Ociosa { idle_min: 95.4 }, "sem atividade ha 95 min"),
            (Reason::Loop { repeats: 4 }, "chamada repetida 4x"),
            (
                Reason::ContextoUrgente {
                    ctx_pct: 91.4,
                    limite_pct: 90,
                },
                "contexto 91% >= 90%",
            ),
            (
                Reason::ContextoAlto {
                    ctx_pct: 55.0,
                    limite_pct: 50,
                },
                "contexto 55% >= 50%",
            ),
            (
                Reason::Releitura {
                    share: 0.643,
                    limite: 0.6,
                    piso: 180_000,
                    turno: 18_500,
                    payback: Some(4.2),
                },
                "releitura 64% (>= 60%) com contexto acima do piso de 180.0k; cada turno rele \
                 18.5k; sessao nova se paga em 4.2 turno(s)",
            ),
            (
                Reason::HandoffVelho { age_min: 300.0 },
                "ultimo handoff ha 5h",
            ),
        ];
        for (r, want) in cases {
            assert_eq!(reason_text(&r), want);
        }
    }

    #[test]
    fn summary_line_is_the_summary_cockpit() {
        let mut s = Summary {
            cockpit: CockpitCounts {
                continuar: 20,
                compactar: 2,
                handoff: 3,
                ociosa: 4,
                loop_: 0,
                sem_dado: 0,
            },
            sessions_burn_weighted: Some(41_234_567),
            cache_hit: Some(0.82),
            ..Summary::default()
        };
        assert_eq!(
            summary_line(&s),
            "3 handoff, 2 compactar, 20 continuar, 4 ociosas | burn 41.2M | cache 82%"
        );
        s.cockpit.ociosa = 1;
        s.cockpit.loop_ = 1;
        s.cockpit.sem_dado = 2;
        s.cockpit_pending = 1;
        s.sessions_burn_weighted = None;
        s.cache_hit = None;
        assert_eq!(
            summary_line(&s),
            "3 handoff, 2 compactar, 20 continuar, 1 ociosa, 1 loop, 2 sem dado (1 lendo) \
             | burn - | cache -"
        );
    }

    fn cockpit(ctx: u64, window: u64, idle: f64, handoff: Option<(&str, f64)>) -> Cockpit {
        let t = Tokens {
            input: 1_000,
            cache_write: 20_000,
            cache_read: 3_000_000,
            output: 2_000,
            ctx,
            window,
            window_inferred: false,
        };
        Cockpit::compute(&Thresholds::default(), &t, Some(idle), handoff)
    }

    /// Sessoes 20, 30, 40 e 50 (claude) e 60 (herdr) num `/proc` falso.
    fn snap_with(overlays: Vec<(u32, SessionOverlay)>) -> Snapshot {
        let dir = TempDir::new("ac-cockpit").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        for pid in [20, 30, 40, 50] {
            fp.add(&FakeProcess::new(pid, 1, "claude")).unwrap();
        }
        fp.add(&FakeProcess::new(60, 1, "herdr")).unwrap();
        let mut snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        for (pid, o) in overlays {
            let s = snap.sessions.iter_mut().find(|s| s.pid == pid).unwrap();
            s.overlay = Some(o);
        }
        snap
    }

    fn with_cockpit(c: Cockpit) -> SessionOverlay {
        SessionOverlay {
            cockpit: Some(c),
            ..Default::default()
        }
    }

    #[test]
    fn rows_follow_the_script_order_and_hide_idle_without_all() {
        let snap = snap_with(vec![
            (20, with_cockpit(cockpit(100_000, 1_000_000, 1.0, None))),
            (30, with_cockpit(cockpit(950_000, 1_000_000, 1.0, None))),
            (40, with_cockpit(cockpit(500_000, 1_000_000, 200.0, None))),
            (
                50,
                SessionOverlay {
                    cockpit_pending: true,
                    ..Default::default()
                },
            ),
        ]);
        let out = render(&snap, false, None);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].starts_with("REC"), "{out}");
        // 30 (95 %) antes de 20 (10 %); a pendente no fim; a ociosa (40) e o herdr fora.
        assert!(lines[2].starts_with("HANDOFF"), "{out}");
        assert!(lines[2].contains("950.0k/1.0M 95%"), "{out}");
        assert!(
            lines[3]
                .trim_start()
                .starts_with("motivo: contexto 95% >= 90%"),
            "{out}"
        );
        assert!(lines[4].starts_with("CONTINUAR"), "{out}");
        assert!(lines[4].contains("100.0k/1.0M 10%"), "{out}");
        assert!(lines[5].starts_with("lendo"), "{out}");
        assert_eq!(lines.len(), 6, "{out}");
        assert!(!out.contains("OCIOSA") && !out.contains("herdr"), "{out}");

        let all = render(&snap, true, None);
        assert!(all.contains("OCIOSA"), "{all}");
        assert!(all.contains("motivo: sem atividade ha 200 min"), "{all}");
        assert!(!all.contains("herdr"), "{all}");
    }

    #[test]
    fn hostile_text_from_process_herdr_and_transcript_stays_on_one_line() {
        let evil = "a\x1b[2J\x07b\nTOTAL fake\r\u{9b}c";
        let mut c = cockpit(600_000, 1_000_000, 1.0, Some(("h.md", 300.0)));
        c.handoff_name = Some(format!("x\x1b]0;pwn\x07\n{evil}"));
        let mut snap = snap_with(vec![
            (
                20,
                SessionOverlay {
                    name: Some(evil.into()),
                    model: Some(evil.into()),
                    provider: Some(evil.into()),
                    cockpit: Some(c.clone()),
                    ..Default::default()
                },
            ),
            (
                30,
                SessionOverlay {
                    herdr: Some(HerdrOverlay {
                        title: Some(format!("titulo {evil}")),
                        ..Default::default()
                    }),
                    cockpit: Some(cockpit(10_000, 1_000_000, 1.0, None)),
                    ..Default::default()
                },
            ),
        ]);
        snap.sessions.iter_mut().find(|s| s.pid == 40).unwrap().cwd =
            Some(format!("/home/dev/{evil}"));
        let out = render(&snap, true, Some("/home/dev"));
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\n'),
            "byte de controle na saida: {out:?}"
        );
        // Resumo, cabecalho, 20 + motivo, 30, 40, 50.
        assert_eq!(out.lines().count(), 7, "{out}");
        let escaped = r"a\x1b[2J\x07b\nTOTAL fake\r\u{9b}c";
        assert!(out.contains(&format!("titulo {escaped}")), "{out}");
        assert!(out.contains(&format!("~/{escaped}")), "{out}");
        assert!(out.contains(r"5h x\x1b]0;pwn\x07\na\x1b"), "{out}");
        assert!(
            out.contains("motivo: contexto 60% >= 50%; ultimo handoff ha 5h"),
            "{out}"
        );
        // Nome, modelo e provider: tres celulas escapadas na linha da 20.
        let row20 = out.lines().find(|l| l.contains("COMPACTAR")).unwrap();
        assert_eq!(row20.matches(escaped).count(), 3, "{row20}");
    }

    #[test]
    fn no_agent_session_says_so_after_the_summary() {
        let dir = TempDir::new("ac-cockpit-empty").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        fp.add(&FakeProcess::new(60, 1, "herdr")).unwrap();
        let snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        assert_eq!(
            render(&snap, true, None),
            "0 handoff, 0 compactar, 0 continuar, 0 ociosas | burn - | cache -\n\
             nenhuma sessao no recorte\n"
        );
    }
}
