//! Cockpit das sessoes de IA: as contas do handoff, a recomendacao, o provider
//! e a idade do ultimo handoff do projeto.
//!
//! Funcoes puras, sem ligacao com o sampler: o overlay de transcripts as usa
//! (C21, ver [`session`]). A fonte das formulas
//! e dos limiares e o `contexto.py` do squad (`skills/contexto-de-sessao/scripts`,
//! funcoes `contas`, `analyse` e `latest_handoff`): para a mesma entrada, o mesmo
//! resultado, na mesma ordem de operacoes de ponto flutuante. Nada aqui
//! reinterpreta o script; onde ele tem um comportamento estranho, o modulo o
//! copia e documenta (ver [`handoff`]).
//!
//! Duas medidas de cache, com nomes distintos de proposito:
//! - [`cache_hit`]: `cache_read / (input + cache_write + cache_read)`, em tokens
//!   brutos, o que o dono pediu para ver;
//! - [`Contas::share`]: a parcela de releitura do script, ponderada
//!   (`0,1*cache_read / burn`). E ela, nao o `cache_hit`, que decide o HANDOFF.

pub mod handoff;
pub mod session;

pub use handoff::{Handoff, HandoffIndex, latest_handoff};
pub use session::{Cockpit, Loops, Tokens, clean_name};

use serde::{Deserialize, Serialize};

/// Peso do cache write em relacao a um token de entrada.
pub const W_WRITE: f64 = 1.25;
/// Peso do cache read.
pub const W_READ: f64 = 0.1;
/// Peso de um token de saida.
pub const W_OUT: f64 = 5.0;
/// % da janela a partir do qual o veredito sobe para COMPACTAR.
pub const HANDOFF_SHOW: u32 = 50;
/// Tokens de saida para escrever o handoff.
pub const HANDOFF_OUT: u64 = 2_000;
/// Minutos sem atividade a partir dos quais a sessao e OCIOSA (estritamente maior).
pub const IDLE_MIN: f64 = 90.0;
/// Vezes que a mesma chamada (ferramenta e argumentos) repete para ser LOOP.
pub const LOOP_REPEATS: u32 = 3;
/// Minutos a partir dos quais o ultimo handoff e velho (estritamente maior).
pub const HANDOFF_OLD_MIN: f64 = 240.0;

/// Limiares que o script le do ambiente, com os mesmos padroes e clamps.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Thresholds {
    /// `SQUAD_CTX_PCT`, padrao 90, em [10, 100]: % da janela que e HANDOFF direto.
    pub handoff_now: u32,
    /// `SQUAD_REREAD`, padrao 60, em [10, 95], dividido por 100.
    pub reread_share: f64,
    /// `SQUAD_REREAD_PISO`, padrao 70, em [0, 100]: piso minimo da releitura, % da janela.
    pub reread_floor_pct: u32,
    /// `CLAUDE_SESSION_BUDGET`, padrao 20M, em [1M, 1e10]: orcamento de burn da sessao.
    pub budget: u64,
    /// `SQUAD_BURN_CHEIO`, padrao 50, em [1, 100]: burn % em que o piso chega ao minimo.
    pub burn_full_pct: u32,
    /// `SQUAD_CTX_BASE`, padrao 40000, em [1000, 500000]: contexto de uma sessao nova.
    pub ctx_base: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::from_env(|_| None)
    }
}

impl Thresholds {
    /// Le os limiares por `get` (o nome da variavel para o valor). Vazio ou
    /// invalido vira o padrao, como o `env_num` do script.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let num = |name: &str, default: i128, lo: i128, hi: i128| -> i128 {
            let raw = get(name).unwrap_or_default();
            let value = if raw.is_empty() {
                default
            } else {
                py_int(&raw).unwrap_or(default)
            };
            value.clamp(lo, hi)
        };
        Self {
            handoff_now: num("SQUAD_CTX_PCT", 90, 10, 100) as u32,
            reread_share: num("SQUAD_REREAD", 60, 10, 95) as f64 / 100.0,
            reread_floor_pct: num("SQUAD_REREAD_PISO", 70, 0, 100) as u32,
            budget: num(
                "CLAUDE_SESSION_BUDGET",
                20_000_000,
                1_000_000,
                10_i128.pow(10),
            ) as u64,
            burn_full_pct: num("SQUAD_BURN_CHEIO", 50, 1, 100) as u32,
            ctx_base: num("SQUAD_CTX_BASE", 40_000, 1_000, 500_000) as u64,
        }
    }

    /// Os limiares do ambiente do processo.
    pub fn from_process_env() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// As contas do handoff, em tokens ponderados; copia de `contexto.contas`.
    ///
    /// `window` = 0 (sub-sessao, janela desconhecida) deixa o piso em
    /// `2*ctx_base`. Os arredondamentos sao os do script: `int()` trunca,
    /// `round()` e o do Python (ver [`round_n`]) e `trocar` usa a parcela sem
    /// arredondar.
    pub fn contas(&self, inp: u64, wr: u64, rd: u64, out: u64, ctx: u64, window: u64) -> Contas {
        let novo = inp as f64 + wr as f64 * W_WRITE + out as f64 * W_OUT;
        let relei = rd as f64 * W_READ;
        let saving = (ctx as i128 - self.ctx_base as i128) as f64 * W_READ;
        let restart = self.ctx_base as f64 * W_WRITE + HANDOFF_OUT as f64 * W_OUT;
        let share = if novo + relei != 0.0 {
            relei / (novo + relei)
        } else {
            0.0
        };
        let burn_pct = 100.0 * (novo + relei) / self.budget as f64;
        let now = self.handoff_now as f64;
        let low = self.reread_floor_pct.min(self.handoff_now);
        let floor_pct = now - (now - low as f64) * (burn_pct / self.burn_full_pct as f64).min(1.0);
        let base2 = 2 * self.ctx_base;
        let piso_min = base2.max(((window as u128 * low as u128) as f64 / 100.0) as u64);
        let piso = base2.max((window as f64 * floor_pct / 100.0) as u64);
        Contas {
            novo: novo as u64,
            relei: relei as u64,
            burn: (novo + relei) as u64,
            burn_pct: round_n(burn_pct, 1),
            razao: if novo != 0.0 {
                round_n(relei / novo, 3)
            } else {
                0.0
            },
            share: round_n(share, 3),
            piso,
            piso_min,
            turno: (ctx as f64 * W_READ) as u64,
            reinicio: restart as u64,
            payback: (saving > 0.0).then(|| round_n(restart / saving, 1)),
            trocar: share >= self.reread_share && ctx >= piso,
        }
    }

    /// Veredito e motivos, na precedencia de `analyse()` do script.
    ///
    /// `ctx_pct` vai sem arredondar (ver [`ctx_pct`]): 89,996 % e COMPACTAR,
    /// embora o script o exiba como 90.0. `loop_repeats` e o sinal de loop (o
    /// maior numero de chamadas repetidas, [`Loops::repeats`], C23): LOOP vale
    /// sobre OCIOSA e cede ao contexto, como no script.
    /// Os arquivos relidos do script ficam de fora: exigem o caminho do Read.
    pub fn recommend(
        &self,
        ctx_pct: f64,
        contas: &Contas,
        idle_min: Option<f64>,
        handoff_age_min: Option<f64>,
        loop_repeats: Option<u32>,
    ) -> (Recommendation, Vec<Reason>) {
        let mut verdict = Recommendation::Continuar;
        let mut why = Vec::new();
        if let Some(idle) = idle_min.filter(|&m| m > IDLE_MIN) {
            verdict = Recommendation::Ociosa;
            why.push(Reason::Ociosa { idle_min: idle });
        }
        if let Some(repeats) = loop_repeats {
            verdict = Recommendation::Loop;
            why.push(Reason::Loop { repeats });
        }
        if ctx_pct >= self.handoff_now as f64 {
            verdict = Recommendation::Handoff;
            why.push(Reason::ContextoUrgente {
                ctx_pct,
                limite_pct: self.handoff_now,
            });
        } else if ctx_pct >= HANDOFF_SHOW as f64 && verdict == Recommendation::Continuar {
            verdict = Recommendation::Compactar;
            why.push(Reason::ContextoAlto {
                ctx_pct,
                limite_pct: HANDOFF_SHOW,
            });
        }
        if contas.trocar {
            if matches!(
                verdict,
                Recommendation::Continuar | Recommendation::Compactar
            ) {
                verdict = Recommendation::Handoff;
            }
            why.push(Reason::Releitura {
                share: contas.share,
                limite: self.reread_share,
                piso: contas.piso,
                turno: contas.turno,
                payback: contas.payback,
            });
        }
        if let Some(age) = handoff_age_min
            && ctx_pct >= HANDOFF_SHOW as f64
            && age > HANDOFF_OLD_MIN
        {
            why.push(Reason::HandoffVelho { age_min: age });
        }
        (verdict, why)
    }
}

/// Saida de [`Thresholds::contas`]; os mesmos campos do dicionario do script.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Contas {
    /// entrada + 1,25*cache-write + 5*saida, truncado.
    pub novo: u64,
    /// 0,1*cache-read, truncado.
    pub relei: u64,
    /// novo + relei, truncado.
    pub burn: u64,
    /// burn em % do orcamento, 1 casa.
    pub burn_pct: f64,
    /// relei / novo, 3 casas (0.0 sem novo).
    pub razao: f64,
    /// relei / (novo + relei), 3 casas: o REREAD % do cockpit.
    pub share: f64,
    /// contexto a partir do qual a releitura dispara, pesado pelo burn.
    pub piso: u64,
    /// o piso com burn cheio.
    pub piso_min: u64,
    /// 0,1*ctx: pedagio de cada novo turno.
    pub turno: u64,
    /// 1,25*ctx_base + 5*HANDOFF_OUT: escrever o handoff e recriar o cache.
    pub reinicio: u64,
    /// turnos ate a sessao nova se pagar, 1 casa; `None` com ctx <= ctx_base.
    pub payback: Option<f64>,
    /// share (sem arredondar) >= limiar e ctx >= piso.
    pub trocar: bool,
}

/// Veredito do cockpit; os nomes do script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Recommendation {
    Continuar,
    Compactar,
    Handoff,
    Ociosa,
    Loop,
}

/// Por que o veredito e o que e: so numeros, o texto e de quem exibe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// Sem atividade ha `idle_min` minutos (> [`IDLE_MIN`]).
    Ociosa { idle_min: f64 },
    /// A mesma chamada repetida `repeats` vezes.
    Loop { repeats: u32 },
    /// Contexto em `ctx_pct` >= `limite_pct` (`SQUAD_CTX_PCT`).
    ContextoUrgente { ctx_pct: f64, limite_pct: u32 },
    /// Contexto em `ctx_pct` >= `limite_pct` ([`HANDOFF_SHOW`]).
    ContextoAlto { ctx_pct: f64, limite_pct: u32 },
    /// Releitura acima do limiar com contexto acima do piso.
    Releitura {
        share: f64,
        limite: f64,
        piso: u64,
        turno: u64,
        payback: Option<f64>,
    },
    /// Ultimo handoff ha `age_min` minutos (> [`HANDOFF_OLD_MIN`]), com contexto >= 50 %.
    HandoffVelho { age_min: f64 },
}

/// `100.0 * ctx / window`, sem arredondar; 0.0 com janela desconhecida.
pub fn ctx_pct(ctx: u64, window: u64) -> f64 {
    if window == 0 {
        0.0
    } else {
        100.0 * ctx as f64 / window as f64
    }
}

/// `cache_read / (input + cache_write + cache_read)`, em tokens brutos, 3 casas;
/// `None` sem tokens. Nao e o [`Contas::share`] (ver o topo do modulo).
pub fn cache_hit(inp: u64, wr: u64, rd: u64) -> Option<f64> {
    let total = inp as f64 + wr as f64 + rd as f64;
    (total > 0.0).then(|| round_n(rd as f64 / total, 3))
}

/// `round(x, n)` do Python: o decimal correto mais proximo, empate para o par
/// sobre o valor binario exato (12.25 -> 12.2; 0.35, que e 0.34999..., -> 0.3).
/// O `{:.n}` do Rust formata o valor exato com o mesmo desempate.
pub fn round_n(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// `int(s)` do Python para base 10: espacos nas pontas, sinal opcional e `_`
/// so entre digitos. Digitos fora de ASCII (que o Python aceita) ficam de fora.
/// Valores enormes saturam, o que da o mesmo resultado depois do clamp.
fn py_int(s: &str) -> Option<i128> {
    let s = s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    let (neg, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let bytes = digits.as_bytes();
    if bytes.is_empty() || bytes[0] == b'_' || bytes[bytes.len() - 1] == b'_' {
        return None;
    }
    let mut value: i128 = 0;
    let mut prev_underscore = false;
    for &b in bytes {
        match b {
            b'_' if !prev_underscore => prev_underscore = true,
            b'0'..=b'9' => {
                prev_underscore = false;
                value = value.saturating_mul(10).saturating_add((b - b'0') as i128);
            }
            _ => return None,
        }
    }
    Some(if neg { -value } else { value })
}

/// Familia de um provider canonico, para cor e agrupamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Family {
    Anthropic,
    OpenAI,
    Google,
    Outro,
}

/// A familia do id canonico de [`provider`].
pub fn family(id: &str) -> Family {
    match id {
        "anthropic" => Family::Anthropic,
        "openai" => Family::OpenAI,
        "google" => Family::Google,
        _ => Family::Outro,
    }
}

/// Provider canonico (`[a-z0-9._-]{1,32}`) de uma sessao.
///
/// Ordem: o declarado pelo transcript (o `provider` do pi, o
/// `session_meta.model_provider` do Codex), em minusculas ASCII e com
/// `openai-codex` -> `openai`, se for um id valido; senao o do agente; senao o
/// prefixo do modelo; senao `None`. Declarado fora do alfabeto e descartado
/// inteiro (nao e saneado), e a inferencia segue.
pub fn provider(agent: &str, declared: Option<&str>, model: Option<&str>) -> Option<String> {
    if let Some(id) = declared.and_then(canonical) {
        return Some(id);
    }
    let by_agent = match agent {
        "claude" => Some("anthropic"),
        "codex" => Some("openai"),
        "gemini" => Some("google"),
        "copilot" => Some("github"),
        "kiro" => Some("aws"),
        "cursor-agent" => Some("cursor"),
        _ => None,
    };
    if let Some(id) = by_agent {
        return Some(id.to_owned());
    }
    let model = model?.trim().to_ascii_lowercase();
    const PREFIXES: &[(&str, &str)] = &[
        ("claude-", "anthropic"),
        ("anthropic/", "anthropic"),
        ("gpt-", "openai"),
        ("o1", "openai"),
        ("o3", "openai"),
        ("o4", "openai"),
        ("codex-", "openai"),
        ("openai/", "openai"),
        ("gemini-", "google"),
        ("google/", "google"),
    ];
    PREFIXES
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, id)| (*id).to_owned())
}

fn canonical(declared: &str) -> Option<String> {
    let id = declared.trim().to_ascii_lowercase();
    let id = match id.as_str() {
        "openai-codex" => "openai".to_owned(),
        _ => id,
    };
    let valid = (1..=32).contains(&id.len())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    valid.then_some(id)
}
