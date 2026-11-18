//! Briefing opcional do modelo: as mensagens do chat e o guard da saida, sobre o cliente de
//! loopback de `slm`. Os achados nunca saem da maquina: `Endpoint` nao aceita outro destino.
//! Os nomes do cliente continuam exportados daqui (`Endpoint`, `Message`, `Source`...).
//! Sem regex, a varredura e escrita a mao.

use crate::insights::{Finding, facts, fallback};
pub use crate::slm::{ClientError, Endpoint, EndpointError, Message, Source};
use crate::slm::{Job, complete};
use serde::Serialize;
use std::fmt;
use std::time::Duration;

const SYSTEM: &str = "You brief a developer who runs many AI coding agents on one machine. \
You get numbered findings, most urgent first. Write 2 sentences in plain English. \
Sentence 1: the most urgent problem, with its numbers. \
Sentence 2: what to do first, naming the command if one is given. \
Use only numbers that appear in the findings. No lists, no preamble.";

const EXAMPLE_IN: &str = "1. [high] 3 sessions are past 75% of their context window; the fullest is api (pid 4411) at 91%. \
Action: write a handoff and start a fresh session.\n\
2. [low] 12 idle sessions hold 2.6 GB. Action: agentcenter-cli reclaim.";

const EXAMPLE_OUT: &str = "Three sessions are past 75% of their context window, and api (pid 4411) is at 91%. \
Write a handoff for it and start a fresh session, then free 2.6 GB with agentcenter-cli reclaim.";

/// Frases de quem saiu do papel ou repetiu o prompt.
const OOC: [&str; 11] = [
    "as an ai",
    "language model",
    "i cannot",
    "i can't",
    "i'm unable",
    "instructions",
    "the findings",
    "sentence 1",
    "sentence 2",
    "numbered findings",
    "plain english",
];

const WORDS: [(&str, &str); 12] = [
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
    ("eleven", "11"),
    ("twelve", "12"),
];

const MIN_CHARS: usize = 20;
const MAX_CHARS: usize = 420;

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
    }
}

/// As quatro mensagens: sistema, o exemplo (entrada e saida) e os achados.
pub fn messages(fs: &[Finding]) -> Vec<Message> {
    vec![
        msg("system", SYSTEM),
        msg("user", EXAMPLE_IN),
        msg("assistant", EXAMPLE_OUT),
        msg("user", &facts(fs)),
    ]
}

// ------------------------------------------------------------------- guard

/// Por que a saida do modelo foi recusada (frase curta).
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub reason: String,
}

fn rejected(reason: impl Into<String>) -> Rejection {
    Rejection {
        reason: reason.into(),
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Tira de `<think>` ate `</think>` (ou ate o fim, se nunca fecha): e o raciocinio do modelo,
/// nao a resposta.
fn drop_think(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => {
                out.push(' ');
                rest = &rest[i + j + "</think>".len()..];
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Troca `<|...|>` e `</think>` solto por um espaco.
fn strip_special(text: &str) -> String {
    let text = &drop_think(text);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if let Some(body) = tail.strip_prefix("<|") {
            // <| [^|>]* |>
            let n = body.find(['|', '>']).unwrap_or(body.len());
            body[n..].starts_with("|>").then_some(2 + n + 2)
        } else if tail.starts_with("</think>") {
            Some("</think>".len())
        } else {
            None
        };
        match skip {
            Some(n) => {
                out.push(' ');
                rest = &tail[n..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Tira marcas especiais, fica com o primeiro paragrafo e acha os espacos.
fn clean(text: &str) -> String {
    let stripped = strip_special(text);
    let first = stripped.trim().split("\n\n").next().unwrap_or("");
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Primeiro codepoint (o zero) de cada bloco de digitos decimais Unicode comum.
const ZEROS: [u32; 16] = [
    0x0660, 0x06F0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66, 0x0E50,
    0x0ED0, 0x0F20, 0x1040, 0xFF10,
];

/// O valor decimal de `c`, se tiver um (ASCII, fullwidth, arabe-indico, devanagari...).
fn decimal(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    let u = c as u32;
    ZEROS
        .iter()
        .find(|z| (**z..**z + 10).contains(&u))
        .map(|z| u - z)
}

/// Numeros no texto, com digitos normalizados para ASCII: uma sequencia de digitos com uma parte
/// decimal opcional (`91`, `2.6`, `９９`). Um caractere numerico sem valor decimal (`½`, `Ⅷ`, `²`)
/// vem como `?` e nunca esta nos achados.
fn numbers(text: &str) -> Vec<String> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_numeric() {
            i += 1;
            continue;
        }
        let mut n = String::new();
        let run = |i: &mut usize, n: &mut String| {
            while *i < cs.len() && cs[*i].is_numeric() {
                n.push(
                    decimal(cs[*i])
                        .and_then(|d| char::from_digit(d, 10))
                        .unwrap_or('?'),
                );
                *i += 1;
            }
        };
        run(&mut i, &mut n);
        if i + 1 < cs.len() && cs[i] == '.' && cs[i + 1].is_numeric() {
            n.push('.');
            i += 1;
            run(&mut i, &mut n);
        }
        out.push(n);
    }
    out
}

/// Numeros por extenso sem digito em `WORDS` (zero, duzia, acima de doze): so passam se o texto
/// dos achados tem a mesma palavra.
const BIG: [&str; 23] = [
    "zero",
    "dozen",
    "dozens",
    "billion",
    "trillion",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
];

fn words(low: &str) -> impl Iterator<Item = &str> {
    low.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// O texto com `one`..`twelve` (palavra inteira) trocados por digitos.
fn spell_to_digits(low: &str) -> String {
    let mut out = String::with_capacity(low.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match WORDS.iter().find(|(w, _)| *w == word.as_str()) {
            Some((_, d)) => out.push_str(d),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in low.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Caractere que nao aparece na tela mas muda o que se le ou o que o terminal faz: controle (Cc)
/// e os de formato (Cf) de largura zero, de direcao do texto e as tags.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// Limpa e confere a saida do modelo; devolve o texto limpo ou o motivo da recusa.
/// Ordem: vazio, longo, fora do papel, numero inventado, fato-chave ausente.
pub fn guard(text: &str, fs: &[Finding]) -> Result<String, Rejection> {
    let t = clean(text);
    if t.chars().any(hidden) {
        return Err(rejected("control character"));
    }
    let len = t.chars().count();
    if len < MIN_CHARS {
        return Err(rejected("empty"));
    }
    if len > MAX_CHARS {
        return Err(rejected("too long"));
    }
    let low = t.to_lowercase();
    if let Some(p) = OOC.iter().find(|p| low.contains(**p)) {
        return Err(rejected(format!("out of role: {p}")));
    }
    let known = facts(fs);
    let mut allowed = numbers(&known);
    allowed.extend(["1".to_string(), "2".to_string()]);
    let known_low = known.to_lowercase();
    if let Some(w) = words(&low).find(|w| BIG.contains(w) && !words(&known_low).any(|k| k == *w)) {
        return Err(rejected(format!("number not in findings: {w}")));
    }
    for n in numbers(&t) {
        // zeros finais so contam depois de um ponto decimal (a referencia
        // Python aceitava 20 e 100 porque "2" e "1" sempre estao permitidos)
        let trimmed = if n.contains('.') {
            n.trim_end_matches('0').trim_end_matches('.')
        } else {
            n.as_str()
        };
        if !allowed.contains(&n) && !allowed.iter().any(|a| a == trimmed) {
            return Err(rejected(format!("number not in findings: {n}")));
        }
    }
    // `one`..`twelve` valem pelo digito: passam se o digito esta nos achados (1 e 2 sempre
    // estao, entao "one" de prosa comum nunca recusa) ou se a propria palavra esta la.
    if let Some(w) = words(&low).find(|w| {
        WORDS
            .iter()
            .any(|(word, d)| word == w && !allowed.iter().any(|a| a == d))
            && !words(&known_low).any(|k| k == *w)
    }) {
        return Err(rejected(format!("number not in findings: {w}")));
    }
    if let Some(first) = fs.first() {
        let spelled = spell_to_digits(&low);
        for need in &first.must {
            let need = need.to_lowercase();
            if !low.contains(&need) && !spelled.contains(&need) {
                return Err(rejected(format!("missing key fact: {need}")));
            }
        }
    }
    Ok(t)
}

// ------------------------------------------------------------------ client

/// POST `/v1/chat/completions` no servidor local e devolve `choices[0].message.content`.
/// `timeout` e o prazo total (conexao, escrita e leitura juntas).
pub fn ask(
    endpoint: &Endpoint,
    messages: &[Message],
    timeout: Duration,
) -> Result<String, ClientError> {
    complete(endpoint, &Job::new(messages.to_vec(), timeout))
}

// ------------------------------------------------------------------- brief

/// O briefing mostrado; `rejected` diz por que o modelo nao foi usado (se foi tentado).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Briefing {
    pub text: String,
    pub source: Source,
    pub rejected: Option<String>,
}

/// O texto do modelo se houver endpoint e a resposta passar o guard; senao o texto de regras.
/// Sem endpoint nao ha o que recusar (`rejected` fica vazio); erro do cliente, timeout e recusa
/// do guard trazem o motivo.
pub fn brief(fs: &[Finding], endpoint: Option<&Endpoint>, timeout: Duration) -> Briefing {
    let rules = |rejected: Option<String>| Briefing {
        text: fallback(fs),
        source: Source::Rules,
        rejected,
    };
    let Some(endpoint) = endpoint else {
        return rules(None);
    };
    match ask(endpoint, &messages(fs), timeout) {
        Err(e) => rules(Some(e.to_string())),
        Ok(raw) => match guard(&raw, fs) {
            Ok(text) => Briefing {
                text,
                source: Source::Model,
                rejected: None,
            },
            Err(r) => rules(Some(r.reason)),
        },
    }
}
