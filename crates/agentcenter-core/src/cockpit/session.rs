//! O cockpit de uma sessao: as contas e o veredito prontos para o overlay.
//!
//! [`Cockpit::compute`] junta [`Thresholds::contas`] e
//! [`Thresholds::recommend`] na ordem de `analyse()` do script: a janela e o %
//! saem dos tokens do transcript inteiro (o burn e acumulado: `/compact` nao
//! zera, `/clear` abre arquivo novo), o ocio vem do mtime do transcript e a
//! idade do handoff de [`super::HandoffIndex`]. O LOOP (C23) vem de
//! [`Loops`], as ultimas chamadas de ferramenta do transcript.

use super::{LOOP_REPEATS, Reason, Recommendation, Thresholds, cache_hit, ctx_pct};
use serde::{Deserialize, Serialize};

/// Tamanho maximo de [`Cockpit::handoff_name`], em caracteres.
pub const HANDOFF_NAME_MAX: usize = 120;

/// Tokens de um transcript, crus, como o script os acumula.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Entrada sem cache.
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Contexto do ultimo pedido.
    pub ctx: u64,
    /// Janela usada no % (a declarada ou a inferida).
    pub window: u64,
    /// A janela nao veio do transcript (ou o contexto passou dela).
    pub window_inferred: bool,
}

/// Sinal de loop (C23): o `loops()` do script sobre as ultimas
/// [`crate::overlay::transcript::tools::LOOP_WINDOW`] tool calls do
/// transcript. So os dois numeros; a chamada repetida nao sai daqui.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loops {
    /// Maior sequencia de chamadas identicas seguidas. Sem nenhuma chamada e
    /// 1, como o `best` do script (`streak, best = 1, 1`): a paridade com
    /// `loops()` vale tambem ai. 0 so no [`Loops::default`], que e "sem
    /// sinal" (JSON de antes do C23, [`Cockpit::compute`]), nunca uma conta.
    pub max_streak: u32,
    /// Maior numero de vezes que uma mesma chamada aparece na janela (0 sem
    /// chamadas).
    pub repeated_max: u32,
}

impl Loops {
    /// `rep or streak >= LOOP_REPEATS` do script, com o
    /// `max([n for _, n in rep] + [streak])` do motivo.
    pub fn repeats(&self) -> Option<u32> {
        let (rep, streak) = (self.repeated_max, self.max_streak);
        (rep >= LOOP_REPEATS || streak >= LOOP_REPEATS).then_some(rep.max(streak))
    }
}

/// Metricas de cockpit de uma sessao (`SessionOverlay.cockpit`, schema 5).
///
/// `cache_write` e `ctx_tokens` vao junto para que quem le o JSON refaca as
/// contas (com `tokens_in` e `tokens_out` do overlay) sem o transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cockpit {
    /// Burn ponderado do transcript inteiro (`contas.burn`).
    pub burn_weighted: u64,
    /// Burn em % do orcamento da sessao (`CLAUDE_SESSION_BUDGET`), 1 casa.
    pub burn_pct: f64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// `cache_read / tokens_in`, 3 casas ([`cache_hit`]).
    pub cache_hit: Option<f64>,
    /// Parcela de releitura ponderada (`contas.share`), 3 casas.
    pub reread_share: f64,
    /// `relei / novo` (`contas.razao`), 3 casas.
    pub reread_ratio: f64,
    /// Turnos ate a sessao nova se pagar; `None` com contexto <= `ctx_base`.
    pub payback_turns: Option<f64>,
    /// Piso da releitura (`contas.piso`), em tokens.
    pub floor_tokens: u64,
    /// Contexto do ultimo pedido, em tokens.
    pub ctx_tokens: u64,
    pub window: u64,
    pub window_inferred: bool,
    /// Minutos desde o mtime do transcript (negativo se no futuro).
    pub idle_min: Option<f64>,
    /// Minutos desde o mtime do ultimo handoff do projeto.
    pub handoff_age_min: Option<f64>,
    /// Nome do arquivo do handoff, saneado ([`clean_name`]).
    pub handoff_name: Option<String>,
    /// [`Loops::max_streak`] (C23): 1 ou mais quando o transcript foi lido
    /// (1 tambem sem nenhuma chamada, como o script); 0 e "sem sinal": JSON de
    /// antes do C23 (o campo falta e le 0) ou [`Cockpit::compute`].
    #[serde(default)]
    pub max_streak: u32,
    /// [`Loops::repeated_max`] (C23): 0 sem chamadas ou sem sinal; JSON sem o
    /// campo le 0.
    #[serde(default)]
    pub repeated_max: u32,
    pub recommendation: Recommendation,
    pub reasons: Vec<Reason>,
}

impl Cockpit {
    /// As contas e o veredito de `t`, sem sinal de loop; `handoff` e (nome,
    /// idade em minutos).
    pub fn compute(
        th: &Thresholds,
        t: &Tokens,
        idle_min: Option<f64>,
        handoff: Option<(&str, f64)>,
    ) -> Self {
        Self::compute_with_loops(th, t, Loops::default(), idle_min, handoff)
    }

    /// [`Cockpit::compute`] com o sinal de loop do transcript.
    pub fn compute_with_loops(
        th: &Thresholds,
        t: &Tokens,
        loops: Loops,
        idle_min: Option<f64>,
        handoff: Option<(&str, f64)>,
    ) -> Self {
        let c = th.contas(
            t.input,
            t.cache_write,
            t.cache_read,
            t.output,
            t.ctx,
            t.window,
        );
        let age = handoff.map(|(_, age)| age);
        let (recommendation, reasons) =
            th.recommend(ctx_pct(t.ctx, t.window), &c, idle_min, age, loops.repeats());
        Self {
            burn_weighted: c.burn,
            burn_pct: c.burn_pct,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            cache_hit: cache_hit(t.input, t.cache_write, t.cache_read),
            reread_share: c.share,
            reread_ratio: c.razao,
            payback_turns: c.payback,
            floor_tokens: c.piso,
            ctx_tokens: t.ctx,
            window: t.window,
            window_inferred: t.window_inferred,
            idle_min,
            handoff_age_min: age,
            handoff_name: handoff.map(|(name, _)| clean_name(name)),
            max_streak: loops.max_streak,
            repeated_max: loops.repeated_max,
            recommendation,
            reasons,
        }
    }
}

/// Nome vindo do disco, pronto para uma linha: sequencias ANSI (CSI) saem
/// inteiras, quebras de linha e tabs viram espaco, os demais caracteres de
/// controle (C0, DEL, C1) saem, e o resultado tem no maximo
/// [`HANDOFF_NAME_MAX`] caracteres.
pub fn clean_name(name: &str) -> String {
    let mut out = String::new();
    let mut chars = name.chars().peekable();
    let mut n = 0;
    while let Some(c) = chars.next() {
        if n == HANDOFF_NAME_MAX {
            break;
        }
        if c == '\x1b' && chars.peek() == Some(&'[') {
            // CSI: parametros e intermediarios ate o byte final (@ a ~).
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        out.push(c);
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_name_drops_ansi_and_control_and_caps_length() {
        assert_eq!(clean_name("a\x1b[31mb\x1b[0m.md"), "ab.md");
        assert_eq!(clean_name("a\nb\rc\td"), "a b c d");
        assert_eq!(clean_name("x\x07\x7f\u{9b}y"), "xy");
        assert_eq!(clean_name(&"é".repeat(200)).chars().count(), 120);
    }
}
