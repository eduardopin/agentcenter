//! Tabela de texto: uma linha de resumo no topo ([`summary_line`]), uma linha
//! por sessao (ordem do snapshot: PSS+SwapPss desc) e uma linha final com os
//! totais da maquina.
//!
//! Valor ausente sai como `-`; soma que nao cobre todos os processos da sessao
//! (ou todas as sessoes, no total) sai com `*` (ver `Session::partial`).
//! Toda celula passa por [`sanitize`] antes do calculo de largura: cwd, cmdline e
//! os campos de overlay vem de fora e nao podem levar escape de terminal.

use agentcenter_core::session::Metric;
use agentcenter_core::{IoStallSource, Session, Snapshot};
use std::borrow::Cow;
use std::fmt::Write;

const HEADERS: [&str; 14] = [
    "AGENT", "PID", "PROCS", "CPU%", "PSS", "SWAP", "READ/s", "WRITE/s", "STALL", "FLAG", "NAME",
    "STATUS", "CTX", "CWD",
];

/// Bytes em unidade binaria curta: `512B`, `12.0K`, `340M`, `1.2G`.
pub fn human_bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = b.max(0.0);
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{v:.0}{}", UNITS[u])
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// Categoria Unicode Cf (formato: bidi, largura zero, BOM, tags) e os
/// separadores de linha e paragrafo (Zl, Zp). Invisiveis ou reordenam o texto
/// (U+202E inverte a direcao do resto da linha); `std` nao expoe a categoria.
fn is_format_or_separator(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn needs_escape(c: char) -> bool {
    c.is_control() || is_format_or_separator(c)
}

/// Escapa caracteres de controle (`c.is_control()`: C0, DEL e C1), de formato
/// (Cf) e separadores de linha/paragrafo para texto visivel: `\n`, `\r`, `\t`,
/// `\x1b`, `\u{9b}`, `\u{202e}`. Uma celula vira sempre uma linha sem
/// sequencia de terminal nem texto invertido.
pub fn sanitize(cell: &str) -> Cow<'_, str> {
    if !cell.chars().any(needs_escape) {
        return Cow::Borrowed(cell);
    }
    let mut out = String::with_capacity(cell.len() + 8);
    for c in cell.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() && (c as u32) < 0x80 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if needs_escape(c) => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// `-` se ausente; `*` no fim se a soma e parcial.
fn cell(v: Option<String>, partial: bool) -> String {
    match v {
        None => "-".to_string(),
        Some(s) if partial => s + "*",
        Some(s) => s,
    }
}

fn metric<T>(s: &Session, m: Metric, v: Option<T>, fmt: impl Fn(T) -> String) -> String {
    cell(v.map(fmt), s.partial.contains(&m))
}

fn pct(v: f64) -> String {
    format!("{v:.1}")
}

fn bytes(v: impl Into<f64>) -> String {
    human_bytes(v.into())
}

fn stall(s: &Session) -> String {
    match (s.stall_pct, s.threads_d) {
        (Some(p), _) => cell(
            Some(format!("{p:.0}%")),
            s.partial.contains(&Metric::StallPct),
        ),
        (None, Some(d)) => format!("D:{d}"),
        (None, None) => "-".to_string(),
    }
}

fn dash<T: ToString>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_string(), |x| x.to_string())
}

/// `c` e `h` ou fica dentro de `h`. `h` vazio (HOME=/ ou HOME vazio) nunca abrevia.
fn under_home(c: &str, h: &str) -> bool {
    !h.is_empty()
        && c.strip_prefix(h)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn row(s: &Session, home: Option<&str>) -> [String; 14] {
    let o = s.overlay.as_ref();
    let cwd = match (&s.cwd, home) {
        // Fronteira de diretorio: HOME=/home/dev nao abrevia /home/devops.
        (Some(c), Some(h)) if under_home(c, h.trim_end_matches('/')) => {
            format!("~{}", &c[h.trim_end_matches('/').len()..])
        }
        (Some(c), _) => c.clone(),
        (None, _) => "-".to_string(),
    };
    [
        s.agent.clone(),
        s.pid.to_string(),
        s.procs.to_string(),
        metric(s, Metric::CpuPct, s.cpu_pct, pct),
        metric(s, Metric::PssBytes, s.pss_bytes, |b| bytes(b as f64)),
        metric(s, Metric::SwapPssBytes, s.swap_pss_bytes, |b| {
            bytes(b as f64)
        }),
        metric(s, Metric::ReadBps, s.read_bps, bytes),
        metric(s, Metric::WriteBps, s.write_bps, bytes),
        stall(s),
        if s.io_bound { "IO" } else { "-" }.to_string(),
        dash(o.and_then(|o| o.name.clone())),
        dash(
            o.and_then(|o| o.status)
                .map(|st| format!("{st:?}").to_lowercase()),
        ),
        dash(o.and_then(|o| o.context_pct).map(|c| format!("{c:.0}%"))),
        cwd,
    ]
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Linha de resumo do topo, de `snap.summary` (calculado no tick, sem leitura
/// nova): `31 sessions (claude 24, codex 5, pi 2) | 4 busy, 2 blocked | 12
/// active processes`. Sessoes e estados so de agentes (herdr fora); agentes em
/// ordem alfabetica, estavel entre frames do `watch`; processos ativos sao os
/// filhos trabalhando (`working_child_processes`). Passa por [`sanitize`]: o
/// nome do agente pode vir de `AGENTCENTER_EXTRA_AGENTS`.
pub fn summary_line(snap: &Snapshot) -> String {
    let s = &snap.summary;
    let mut line = count(s.sessions, "session", "sessions");
    if !s.by_agent.is_empty() {
        let agents: Vec<String> = s.by_agent.iter().map(|(a, n)| format!("{a} {n}")).collect();
        let _ = write!(line, " ({})", agents.join(", "));
    }
    let _ = write!(
        line,
        " | {} busy, {} blocked | {}",
        s.status.busy,
        s.status.blocked,
        count(
            s.working_child_processes,
            "active process",
            "active processes"
        ),
    );
    sanitize(&line).into_owned()
}

/// Total de uma metrica sobre as sessoes: parcial se alguma sessao e parcial
/// nela ou nao tem o valor.
fn total<T: std::iter::Sum<T>>(
    snap: &Snapshot,
    m: Metric,
    f: impl Fn(&Session) -> Option<T>,
    fmt: impl Fn(T) -> String,
) -> String {
    let known = snap.sessions.iter().filter(|s| f(s).is_some()).count();
    let partial =
        known < snap.sessions.len() || snap.sessions.iter().any(|s| s.partial.contains(&m));
    let sum = (known > 0).then(|| snap.sessions.iter().filter_map(&f).sum());
    cell(sum.map(fmt), partial)
}

pub fn render(snap: &Snapshot, home: Option<&str>) -> String {
    let mut rows: Vec<[String; 14]> = vec![HEADERS.map(String::from)];
    rows.extend(snap.sessions.iter().map(|s| row(s, home)));
    let m = &snap.machine;
    let stall_total = match m.io_stall_source {
        IoStallSource::Delayacct => "delayacct".to_string(),
        IoStallSource::DState => format!(
            "D:{}",
            snap.sessions
                .iter()
                .filter_map(|s| s.threads_d)
                .sum::<u32>()
        ),
    };
    let mut t: [String; 14] = std::array::from_fn(|_| String::new());
    t[0] = format!("TOTAL {}", snap.sessions.len());
    t[1] = "-".into();
    t[2] = format!("{}/{}", m.session_processes, m.processes);
    t[3] = total(snap, Metric::CpuPct, |s| s.cpu_pct, pct);
    t[4] = total(snap, Metric::PssBytes, |s| s.pss_bytes, |b| bytes(b as f64));
    t[5] = total(
        snap,
        Metric::SwapPssBytes,
        |s| s.swap_pss_bytes,
        |b| bytes(b as f64),
    );
    t[6] = total(snap, Metric::ReadBps, |s| s.read_bps, bytes);
    t[7] = total(snap, Metric::WriteBps, |s| s.write_bps, bytes);
    t[8] = stall_total;
    t[9] = snap
        .sessions
        .iter()
        .filter(|s| s.io_bound)
        .count()
        .to_string();
    let used = |total: Option<u64>, free: Option<u64>| {
        let (t, f) = (total?, free?);
        Some(format!(
            "{}/{}",
            human_bytes(t.saturating_sub(f) as f64),
            human_bytes(t as f64)
        ))
    };
    let psi = |r: Option<agentcenter_core::PsiResource>| {
        dash(r.and_then(|r| r.some_avg10).map(|v| format!("{v:.1}%")))
    };
    t[13] = format!(
        "machine: {} cores, {}% cpu, mem {}, swap {}, psi some10 cpu {} mem {} io {}",
        m.cores,
        dash(m.cpu_pct.map(pct)),
        dash(used(m.mem_total_bytes, m.mem_available_bytes)),
        dash(used(m.swap_total_bytes, m.swap_free_bytes)),
        psi(m.psi.and_then(|p| p.cpu)),
        psi(m.psi.and_then(|p| p.memory)),
        psi(m.psi.and_then(|p| p.io)),
    );
    rows.push(t);

    // Ponto unico de saida: nada chega ao terminal sem passar por aqui.
    for r in &mut rows {
        for c in r.iter_mut() {
            if let Cow::Owned(clean) = sanitize(c) {
                *c = clean;
            }
        }
    }

    let mut widths = [0usize; 14];
    for r in &rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let mut out = summary_line(snap);
    out.push('\n');
    for r in &rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            // Numeros a direita (PID..STALL), texto a esquerda; ultima coluna sem preenchimento.
            if i == 13 {
                line.push_str(c);
            } else if (1..=8).contains(&i) {
                let _ = write!(line, "{c:>w$}  ", w = widths[i]);
            } else {
                let _ = write!(line, "{c:<w$}  ", w = widths[i]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
    use agentcenter_core::overlay::{OverlayStatus, SessionOverlay};
    use agentcenter_core::{Registry, Sampler, Summary};
    use std::collections::BTreeMap;

    #[test]
    fn control_chars_from_cwd_and_overlay_are_escaped_and_rows_stay_one_line() {
        let dir = TempDir::new("ac-table").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        let mut evil = FakeProcess::new(20, 1, "claude");
        evil.pss_kb = 9000;
        evil.cwd = Some("/home/dev/a\x1b[2J\x07b\nTOTAL fake".into());
        fp.add(&evil).unwrap();
        fp.add(&FakeProcess::new(30, 1, "claude")).unwrap();
        let mut snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        assert_eq!(snap.sessions[0].pid, 20);
        // Campo de overlay (texto de fora) herda a mesma regra.
        snap.sessions[1].overlay = Some(SessionOverlay {
            name: Some("x\x1b]0;pwn\x07\r\u{9b}y".into()),
            ..Default::default()
        });

        let out = render(&snap, Some("/home/dev"));
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\n'),
            "byte de controle na saida: {out:?}"
        );
        // Resumo, cabecalho, uma por sessao e TOTAL.
        assert_eq!(out.lines().count(), snap.sessions.len() + 3, "{out}");
        assert!(out.contains(r"~/a\x1b[2J\x07b\nTOTAL fake"), "{out}");
        assert!(out.contains(r"x\x1b]0;pwn\x07\r\u{9b}y"), "{out}");
        // Largura medida no texto escapado: colunas seguintes continuam alinhadas.
        let lines: Vec<&str> = out.lines().collect();
        let cwd_col = lines[1].find("CWD").unwrap();
        assert_eq!(lines[2].find('~'), Some(cwd_col), "{out}");
        assert_eq!(sanitize("limpo"), Cow::Borrowed("limpo"));
    }

    #[test]
    fn missing_is_dash_and_partial_sum_has_star() {
        let dir = TempDir::new("ac-table-missing").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        fp.add(&FakeProcess::new(20, 1, "claude")).unwrap();
        let root = fp.proc_root();
        let mut sampler = Sampler::new(Registry::default());
        // Primeiro tick: sem CPU nem disco; PSS presente e completo.
        let first = render(&sampler.tick(&root).unwrap(), None);
        let row: Vec<&str> = first.lines().nth(2).unwrap().split_whitespace().collect();
        assert_eq!(
            &row[..8],
            ["claude", "20", "1", "-", "1.0M", "0B", "-", "-"]
        );
        assert!(first.contains("machine: 16 cores, -% cpu"), "{first}");

        // Segundo tick: processo novo na sessao -> CPU e disco parciais.
        fp.set_cpu(16, 100, 1500).unwrap();
        fp.add(&FakeProcess::new(21, 20, "bash")).unwrap();
        let second = render(&sampler.tick(&root).unwrap(), None);
        let lines: Vec<&str> = second.lines().collect();
        let row: Vec<&str> = lines[2].split_whitespace().collect();
        assert_eq!(
            &row[..8],
            ["claude", "20", "2", "0.0*", "2.0M", "0B", "0B*", "0B*"]
        );
        let total: Vec<&str> = lines[3].split_whitespace().collect();
        assert_eq!(
            &total[..9],
            ["TOTAL", "1", "-", "2/2", "0.0*", "2.0M", "0B", "0B*", "0B*"]
        );
    }

    #[test]
    fn total_is_partial_when_a_session_has_no_value_and_sums_the_known() {
        // Duas sessoes: 20 com CPU medida, 30 nova no 2o tick (sem anterior).
        let dir = TempDir::new("ac-table-total").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        let mut a = FakeProcess::new(20, 1, "claude");
        fp.add(&a).unwrap();
        let root = fp.proc_root();
        let mut sampler = Sampler::new(Registry::default());
        sampler.tick(&root).unwrap();
        fp.set_cpu(16, 100, 1500).unwrap();
        a.utime = 50; // 50 ticks em 1 s de parede = 50 % de um nucleo
        fp.add(&a).unwrap();
        fp.add(&FakeProcess::new(30, 1, "claude")).unwrap();
        let snap = sampler.tick(&root).unwrap();
        assert_eq!(snap.sessions.len(), 2);
        let out = render(&snap, None);
        let total: Vec<&str> = out.lines().last().unwrap().split_whitespace().collect();
        // CPU: so a 20 tem valor -> soma dela, com `*`. PSS: as duas -> sem `*`.
        assert_eq!(
            &total[..7],
            ["TOTAL", "2", "-", "2/2", "50.0*", "2.0M", "0B"],
            "{out}"
        );
        // Sessao sem numeros e sem `partial` (job de fundo sem processo vivo):
        // o total continua parcial so por ela faltar.
        let mut bare = snap.clone();
        let b = bare.sessions.iter_mut().find(|s| s.pid == 30).unwrap();
        b.partial.clear();
        assert_eq!(b.cpu_pct, None);
        let out = render(&bare, None);
        let total: Vec<&str> = out.lines().last().unwrap().split_whitespace().collect();
        assert_eq!(total[4], "50.0*", "{out}");
        // Sem nenhuma sessao com valor: `-`, nao 0.
        let mut none = snap.clone();
        for s in &mut none.sessions {
            s.cpu_pct = None;
        }
        let out = render(&none, None);
        let total: Vec<&str> = out.lines().last().unwrap().split_whitespace().collect();
        assert_eq!(total[4], "-", "{out}");
        assert!(out.contains("mem 32G/64G, swap 2.0G/8.0G"), "{out}");
        assert!(out.contains("psi some10 cpu - mem - io -"), "{out}");
    }

    #[test]
    fn home_prefix_needs_a_directory_boundary() {
        let dir = TempDir::new("ac-table-home").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        for (pid, cwd) in [
            (20, "/home/devops/x"),
            (30, "/home/dev"),
            (40, "/home/dev/p"),
        ] {
            let mut p = FakeProcess::new(pid, 1, "claude");
            p.cwd = Some(cwd.into());
            fp.add(&p).unwrap();
        }
        let snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        for home in ["/home/dev", "/home/dev/"] {
            let out = render(&snap, Some(home));
            assert!(out.contains("/home/devops/x"), "{home}: {out}");
            assert!(!out.contains("~ops"), "{home}: {out}");
            assert!(out.contains("~/p"), "{home}: {out}");
            assert!(out.lines().any(|l| l.ends_with('~')), "{home}: {out}");
        }
        // HOME=/ nao vira `~` em tudo.
        let out = render(&snap, Some("/"));
        assert!(!out.contains('~'), "{out}");
    }

    #[test]
    fn format_chars_and_line_separators_are_escaped() {
        assert_eq!(sanitize("a\u{202e}b"), r"a\u{202e}b");
        assert_eq!(sanitize("a\u{2028}b\u{2029}c"), r"a\u{2028}b\u{2029}c");
        assert_eq!(
            sanitize("\u{200b}\u{feff}\u{e0041}"),
            r"\u{200b}\u{feff}\u{e0041}"
        );
        // Texto comum fora do ASCII continua intacto.
        assert_eq!(sanitize("ação 日本"), Cow::Borrowed("ação 日本"));
    }

    #[test]
    fn summary_line_on_top_counts_agents_states_and_working_children() {
        // herdr 10 (filho 11), claude 20 (21 em R), claude 40 (41 em D),
        // codex 30 (31 em S), pi 50.
        let dir = TempDir::new("ac-table-summary").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        for (pid, ppid, comm, state) in [
            (10, 1, "herdr", 'S'),
            (11, 10, "bash", 'S'),
            (20, 1, "claude", 'S'),
            (21, 20, "node", 'R'),
            (30, 10, "codex", 'S'),
            (31, 30, "bash", 'S'),
            (40, 1, "claude", 'S'),
            (41, 40, "cargo", 'D'),
            (50, 1, "pi", 'S'),
        ] {
            let mut p = FakeProcess::new(pid, ppid, comm);
            p.state = state;
            fp.add(&p).unwrap();
        }
        let mut snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        // Estado declarado fixo (o overlay do registry olharia o HOME real).
        for s in &mut snap.sessions {
            let status = match s.pid {
                20 => Some(OverlayStatus::Busy),
                40 => Some(OverlayStatus::Blocked),
                _ => None,
            };
            s.overlay = status.map(|st| SessionOverlay {
                status: Some(st),
                ..Default::default()
            });
        }
        snap.summary = Summary::of(&snap.sessions);

        let out = render(&snap, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "4 sessions (claude 2, codex 1, pi 1) | 1 busy, 1 blocked | 2 active processes",
            "{out}"
        );
        assert!(lines[1].starts_with("AGENT"), "{out}");
        assert!(lines.last().unwrap().starts_with("TOTAL 5"), "{out}");
    }

    #[test]
    fn summary_line_singular_empty_and_escaped() {
        let dir = TempDir::new("ac-table-summary-edge").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        let mut snap = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        assert_eq!(
            summary_line(&snap),
            "0 sessions | 0 busy, 0 blocked | 0 active processes"
        );
        // Nome de agente de fora (AGENTCENTER_EXTRA_AGENTS) nao leva escape de terminal.
        snap.summary = Summary {
            sessions: 1,
            by_agent: BTreeMap::from([("x\x1b[2J\u{202e}y".to_string(), 1)]),
            working_child_processes: 1,
            ..Default::default()
        };
        let line = summary_line(&snap);
        assert_eq!(
            line,
            r"1 session (x\x1b[2J\u{202e}y 1) | 0 busy, 0 blocked | 1 active process"
        );
        assert!(render(&snap, None).starts_with(&format!("{line}\n")));
    }

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(512.0), "512B");
        assert_eq!(human_bytes(1536.0), "1.5K");
        assert_eq!(human_bytes(340.0 * 1024.0 * 1024.0), "340M");
        assert_eq!(human_bytes(1.25 * 1024.0 * 1024.0 * 1024.0), "1.2G");
    }
}
