//! Tool calls de um transcript: total, em voo e por minuto (C14), e o sinal
//! de loop (C23).
//!
//! **So hashes e horarios.** O formato de cada agente diz onde esta o id da
//! chamada e o do resultado ([`super::claude`], [`super::codex`],
//! [`super::pi`]); aqui chega so o hash `u64` do id ([`id_hash`]) e o horario
//! da linha. O resultado nunca e lido.
//!
//! **Loop** (ADR #5, emenda do C23): nome e argumentos da chamada viram, na
//! desserializacao, um hash `u64` da forma canonica de `norm_args` do
//! `contexto.py` ([`name`], [`args`], [`call_key`]); o texto canonico e
//! montado num buffer local, cortado em [`ARGS_MAX_CHARS`] caracteres,
//! passado ao hash e descartado na mesma chamada. Ficam so os
//! [`LOOP_WINDOW`] ultimos hashes por transcript, e dele sai so [`Loops`].
//! Forma canonica = `json.dumps(v, sort_keys=True, ensure_ascii=False)[:400]`:
//! separadores `", "` e `": "`, chaves em ordem de code point (repetida: vale a
//! ultima), so `"`, `\\` e controles < 0x20 escapados (`\n` e afins curtos, o
//! resto `\u00xx`), float no `repr` do Python, e no objeto de fora sem
//! `description`, `timeout` e `run_in_background`. Diferencas conhecidas:
//! inteiro alem de 64 bits sai como float (o Python o escreve inteiro), o
//! serde_json sem `float_roundtrip` erra o ultimo digito de alguns floats
//! longos (`12345678901234567.0`), e
//! nome que nao e texto conta como `"?"`. Argumento que o serde nao le como
//! valor (surrogate solto, numero fora do `f64`, que o Python aceita) nao
//! derruba a linha: ela e lida de novo sem nome e argumentos
//! ([`super::parse_line`]) e a chamada fica fora da janela.
//!
//! - `total`: chamadas vistas (cada bloco de chamada conta, como o `jq` do
//!   card conta).
//! - Em voo: ids de chamada sem resultado ainda, ate [`OPEN_CAP`] por
//!   transcript; passado o teto, sai o mais velho (chamada orfa de um processo
//!   morto nao segura o espaco das novas). Id repetido entra uma vez.
//! - Por minuto: baldes de minuto do relogio de parede (horario da linha da
//!   chamada) dos ultimos [`RATE_MINUTES`] minutos, o atual inclusive;
//!   [`ToolCalls::per_min`] divide pelo tempo que essa janela cobre.

use crate::cockpit::Loops;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::cell::Cell;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hasher};

/// Ids em voo guardados por transcript.
pub const OPEN_CAP: usize = 256;
/// Minutos da media de chamadas por minuto (baldes de 1 min).
pub const RATE_MINUTES: u64 = 5;
const MINUTE_MS: u64 = 60_000;
/// Ultimas chamadas olhadas pelo sinal de loop (`calls[-30:]` do script).
pub const LOOP_WINDOW: usize = 30;
/// Corte da forma canonica dos argumentos, em caracteres (`[:400]`).
pub const ARGS_MAX_CHARS: usize = 400;
/// Chaves de fora que mudam a cada chamada sem mudar o que ela faz.
const VOLATILE_KEYS: [&str; 3] = ["description", "timeout", "run_in_background"];

/// Hash do id de uma chamada. `DefaultHasher::new` tem chaves fixas: o mesmo
/// id da o mesmo hash na chamada e no resultado.
pub fn id_hash(id: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(id);
    h.finish()
}

/// Contagem de tool calls de um transcript.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCalls {
    /// Chamadas vistas.
    pub total: u64,
    /// Hash dos ids sem resultado, do mais velho para o mais novo.
    open: Vec<u64>,
    /// `(minuto desde a epoca, chamadas)`, balde `minuto % RATE_MINUTES`.
    minutes: [(u64, u32); RATE_MINUTES as usize],
    /// Chave ([`call_key`]) das ultimas [`LOOP_WINDOW`] chamadas, em anel.
    recent: [u64; LOOP_WINDOW],
    /// Chaves validas em `recent` (ate [`LOOP_WINDOW`]).
    recent_len: usize,
    /// Proxima posicao de `recent` a escrever (a mais velha, com o anel cheio).
    recent_next: usize,
}

impl ToolCalls {
    /// Uma chamada com id `id` (hash) numa linha de horario `at_ms`.
    pub fn called(&mut self, id: Option<u64>, at_ms: Option<u64>) {
        self.total = self.total.saturating_add(1);
        if let Some(id) = id
            && !self.open.contains(&id)
        {
            if self.open.len() >= OPEN_CAP {
                self.open.remove(0);
            }
            self.open.push(id);
        }
        if let Some(at) = at_ms {
            let m = at / MINUTE_MS;
            let slot = &mut self.minutes[(m % RATE_MINUTES) as usize];
            if slot.0 == m {
                slot.1 = slot.1.saturating_add(1);
            } else if slot.0 < m {
                // O balde era de RATE_MINUTES minutos atras (ou mais): recomeca.
                *slot = (m, 1);
            }
            // Mais velho que o balde: fora de qualquer janela que o inclua.
        }
    }

    /// Uma chamada de chave `key` ([`call_key`]) para o sinal de loop; sai a
    /// mais velha passadas [`LOOP_WINDOW`].
    pub fn repeated(&mut self, key: u64) {
        self.recent[self.recent_next] = key;
        self.recent_next = (self.recent_next + 1) % LOOP_WINDOW;
        self.recent_len = (self.recent_len + 1).min(LOOP_WINDOW);
    }

    /// `loops()` do script sobre a janela: a maior sequencia de chaves iguais
    /// seguidas (1 tambem sem chamadas, como o `best` de la) e a maior
    /// contagem de uma chave.
    pub fn loops(&self) -> Loops {
        let start = (self.recent_next + LOOP_WINDOW - self.recent_len) % LOOP_WINDOW;
        let at = |i: usize| self.recent[(start + i) % LOOP_WINDOW];
        let (mut streak, mut best) = (1u32, 1u32);
        for i in 1..self.recent_len {
            streak = if at(i) == at(i - 1) { streak + 1 } else { 1 };
            best = best.max(streak);
        }
        let mut keys = [0u64; LOOP_WINDOW];
        let keys = &mut keys[..self.recent_len];
        for (i, k) in keys.iter_mut().enumerate() {
            *k = at(i);
        }
        keys.sort_unstable();
        let repeated = keys
            .chunk_by(|a, b| a == b)
            .map(|run| run.len() as u32)
            .max()
            .unwrap_or(0);
        Loops {
            max_streak: best,
            repeated_max: repeated,
        }
    }

    /// Resultado da chamada `id` (hash). Id desconhecido nao muda nada.
    pub fn answered(&mut self, id: u64) {
        if let Some(i) = self.open.iter().rposition(|&x| x == id) {
            self.open.remove(i);
        }
    }

    /// Chamadas sem resultado.
    pub fn open(&self) -> u64 {
        self.open.len() as u64
    }

    /// Chamadas por minuto em `[inicio do minuto now-4, now]`, dividido pelo
    /// tempo coberto (4 min mais a fracao do minuto atual), com uma casa.
    pub fn per_min(&self, now_ms: u64) -> f64 {
        let now = now_ms / MINUTE_MS;
        let lo = now.saturating_sub(RATE_MINUTES - 1);
        let n: u64 = self
            .minutes
            .iter()
            .filter(|(m, _)| (lo..=now).contains(m))
            .map(|&(_, c)| u64::from(c))
            .sum();
        let span = (now - lo) as f64 + (now_ms % MINUTE_MS) as f64 / MINUTE_MS as f64;
        if span <= 0.0 {
            return 0.0;
        }
        super::round1(n as f64 / span)
    }
}

/// Chave do sinal de loop: `(nome, norm_args(argumentos))` do script, de
/// [`name`] e [`args`]. `None` (argumento que nao foi lido) fica fora da
/// janela. `empty` e o `or {}` / `or ""` do formato para argumento falso.
pub fn call_key(name: Option<u64>, args: Args, empty: Empty) -> Option<u64> {
    let args = match args {
        Args::Unread => return None,
        Args::Falsy => id_hash(empty.canon().as_bytes()),
        Args::Hash(h) => h,
    };
    let mut h = DefaultHasher::new();
    h.write_u64(name.unwrap_or_else(|| id_hash(b"?")));
    h.write_u64(args);
    Some(h.finish())
}

/// O que o script poe no lugar de argumento falso (ausente, `null`, `{}`,
/// `""`, `0`, `false`, `[]`).
#[derive(Debug, Clone, Copy)]
pub enum Empty {
    /// `or {}` (Claude e pi).
    Object,
    /// `or ""` (Codex).
    Text,
}

impl Empty {
    fn canon(self) -> &'static str {
        match self {
            Self::Object => "{}",
            Self::Text => "\"\"",
        }
    }
}

/// Argumentos de uma chamada, ja em hash.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Args {
    /// Ausente ou falso para o Python: vale o seguinte do `or`.
    #[default]
    Falsy,
    /// Hash da forma canonica, cortada em [`ARGS_MAX_CHARS`] caracteres.
    Hash(u64),
    /// Linha relida sem argumentos ([`skipping`]): chamada fora da janela.
    Unread,
}

impl Args {
    /// O `a or b` do Python.
    pub fn or(self, other: Args) -> Args {
        match self {
            Args::Falsy => other,
            v => v,
        }
    }
}

// Por que um flag de thread e nao um retorno explicito: o erro de um
// surrogate solto ou de um numero fora do `f64` sai do proprio
// `serde_json::Deserializer` (em `deserialize_any`, antes de [`Canon`] ver o
// valor), e com ele o desserializador da linha fica inutilizavel: [`args`] nao
// tem como devolver `Args::Unread` e seguir. A linha tem de ser lida de novo
// com o campo pulado, e o `deserialize_with` de um campo nao recebe argumento.
// O retorno explicito exigiria structs genericas no modo (`Line<Read>` e
// `Line<Skip>`) nos tres formatos, isto e, reescrever os parsers. O flag e
// seguro porque: (1) e por thread e a leitura de uma linha e sincrona, entao
// duas leituras nao se veem; (2) [`super::parse_line`] zera `TOUCHED` na
// entrada, entao nenhum valor sobra de outra linha; (3) `SKIP` so vale dentro
// de [`skipping`], que o restaura tambem se `f` entrar em panico (guarda), e
// nenhum parser chama `parse_line` de dentro de outro.
thread_local! {
    /// Uma desserializacao de nome ou argumentos comecou desde o ultimo
    /// [`take_touched`].
    static TOUCHED: Cell<bool> = const { Cell::new(false) };
    /// [`skipping`] em curso: nome e argumentos sao pulados.
    static SKIP: Cell<bool> = const { Cell::new(false) };
}

/// Se algum [`name`] ou [`args`] rodou desde a ultima chamada (e zera).
pub(crate) fn take_touched() -> bool {
    TOUCHED.with(|t| t.replace(false))
}

/// Roda `f` com nome e argumentos pulados (como antes do C23).
pub(crate) fn skipping<T>(f: impl FnOnce() -> T) -> T {
    /// Devolve o `SKIP` de antes ao sair, com panico ou sem.
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SKIP.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(SKIP.with(|s| s.replace(true)));
    f()
}

fn skip() -> bool {
    TOUCHED.with(|t| t.set(true));
    SKIP.with(Cell::get)
}

/// Nome da ferramenta em hash; vazio ou de outro tipo vira `None` (o `or "?"`
/// de [`call_key`]).
pub fn name<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    if skip() {
        return IgnoredAny::deserialize(d).map(|_| None);
    }
    let empty = id_hash(b"");
    super::lenient::hash(d).map(|h| h.filter(|&h| h != empty))
}

/// Argumentos em hash da forma canonica ([`Args`]).
pub fn args<'de, D: Deserializer<'de>>(d: D) -> Result<Args, D::Error> {
    if skip() {
        return IgnoredAny::deserialize(d).map(|_| Args::Unread);
    }
    let v = Canon::TOP.deserialize(d)?;
    Ok(if v.falsy {
        Args::Falsy
    } else {
        Args::Hash(id_hash(v.text.s.as_bytes()))
    })
}

/// Texto canonico em construcao, ate [`ARGS_MAX_CHARS`] caracteres.
#[derive(Default)]
struct Text {
    s: String,
    chars: usize,
}

impl Text {
    fn full(&self) -> bool {
        self.chars >= ARGS_MAX_CHARS
    }

    fn push(&mut self, c: char) {
        if !self.full() {
            self.s.push(c);
            self.chars += 1;
        }
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            if self.full() {
                break;
            }
            self.push(c);
        }
    }

    /// `s` como string JSON do Python (`ensure_ascii=False`).
    fn quoted(&mut self, s: &str) {
        self.push('"');
        for c in s.chars() {
            if self.full() {
                return;
            }
            match c {
                '"' => self.push_str("\\\""),
                '\\' => self.push_str("\\\\"),
                '\n' => self.push_str("\\n"),
                '\r' => self.push_str("\\r"),
                '\t' => self.push_str("\\t"),
                '\u{8}' => self.push_str("\\b"),
                '\u{c}' => self.push_str("\\f"),
                c if (c as u32) < 0x20 => {
                    let mut e = String::with_capacity(6);
                    let _ = write!(e, "\\u{:04x}", c as u32);
                    self.push_str(&e);
                }
                c => self.push(c),
            }
        }
        self.push('"');
    }
}

/// Valor canonico: o texto, se o Python o acha falso, e o texto cru quando e
/// chave de objeto (para ordenar).
struct Value {
    text: Text,
    falsy: bool,
    kind: Kind,
}

enum Kind {
    Str(String),
    Other,
}

impl Value {
    fn other(text: &str, falsy: bool) -> Self {
        let mut t = Text::default();
        t.push_str(text);
        Self {
            text: t,
            falsy,
            kind: Kind::Other,
        }
    }
}

/// `repr(float)` do Python: o menor decimal que volta ao mesmo `f64`, em
/// notacao cientifica com o expoente decimal fora de [-4, 16).
fn py_float(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let decpt = exp + 1;
    if !(-4 < decpt && decpt <= 16) {
        let (d1, rest) = digits.split_at(1);
        let dot = if rest.is_empty() { "" } else { "." };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{d1}{dot}{rest}e{esign}{:02}", exp.abs());
    }
    let n = digits.len() as i32;
    if decpt <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt >= n {
        format!("{sign}{digits}{}.0", "0".repeat((decpt - n) as usize))
    } else {
        let (a, b) = digits.split_at(decpt as usize);
        format!("{sign}{a}.{b}")
    }
}

/// Desserializa um valor qualquer na sua forma canonica.
#[derive(Clone, Copy)]
struct Canon {
    /// Objeto de fora: sem [`VOLATILE_KEYS`].
    top: bool,
    /// Chave de objeto: guarda o texto cru (para ordenar).
    key: bool,
}

impl Canon {
    const TOP: Self = Self {
        top: true,
        key: false,
    };
    const INNER: Self = Self {
        top: false,
        key: false,
    };
    const KEY: Self = Self {
        top: false,
        key: true,
    };
}

impl<'de> DeserializeSeed<'de> for Canon {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Canon {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::other(if v { "true" } else { "false" }, !v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::other(&v.to_string(), v == 0))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::other(&v.to_string(), v == 0))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::other(&py_float(v), v == 0.0))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::other("null", true))
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::other("null", true))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        let mut text = Text::default();
        text.quoted(v);
        let kind = if self.key {
            // Cru so ate o corte: chaves iguais nos 400 primeiros caracteres
            // ja passam do corte do texto inteiro.
            let raw = match v.char_indices().nth(ARGS_MAX_CHARS) {
                Some((end, _)) => &v[..end],
                None => v,
            };
            Kind::Str(raw.to_owned())
        } else {
            Kind::Other
        };
        Ok(Value {
            text,
            falsy: v.is_empty(),
            kind,
        })
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Value, A::Error> {
        let mut text = Text::default();
        text.push('[');
        let mut n = 0;
        while !text.full() {
            let Some(v) = s.next_element_seed(Canon::INNER)? else {
                break;
            };
            if n > 0 {
                text.push_str(", ");
            }
            text.push_str(&v.text.s);
            n += 1;
        }
        while s.next_element::<IgnoredAny>()?.is_some() {}
        text.push(']');
        Ok(Value {
            text,
            falsy: n == 0,
            kind: Kind::Other,
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Value, A::Error> {
        let mut entries: Vec<(String, Text)> = Vec::new();
        let mut seen = false;
        while let Some(k) = m.next_key_seed(Canon::KEY)? {
            seen = true;
            let key = match k.kind {
                Kind::Str(raw) => raw,
                Kind::Other => k.text.s,
            };
            if self.top && VOLATILE_KEYS.contains(&key.as_str()) {
                m.next_value::<IgnoredAny>()?;
                continue;
            }
            let v = m.next_value_seed(Canon::INNER)?;
            let mut kv = Text::default();
            kv.quoted(&key);
            kv.push_str(": ");
            kv.push_str(&v.text.s);
            entries.push((key, kv));
        }
        // Estavel: entre chaves repetidas a ultima e a que fica (`dict`).
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut text = Text::default();
        text.push('{');
        let mut first = true;
        for (i, (key, kv)) in entries.iter().enumerate() {
            if entries.get(i + 1).is_some_and(|next| &next.0 == key) {
                continue;
            }
            if text.full() {
                break;
            }
            if !first {
                text.push_str(", ");
            }
            text.push_str(&kv.s);
            first = false;
        }
        text.push('}');
        Ok(Value {
            text,
            falsy: !seen,
            kind: Kind::Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 29_870_093 * MINUTE_MS; // inicio de um minuto de 2026

    #[test]
    fn open_counts_ids_without_result() {
        let mut t = ToolCalls::default();
        t.called(Some(id_hash(b"a")), None);
        t.called(Some(id_hash(b"b")), None);
        t.called(Some(id_hash(b"a")), None); // repetido: uma vez em voo
        assert_eq!((t.total, t.open()), (3, 2));
        t.answered(id_hash(b"a"));
        t.answered(id_hash(b"x")); // desconhecido
        assert_eq!(t.open(), 1);
        t.answered(id_hash(b"b"));
        assert_eq!(t.open(), 0);
        t.called(None, None); // sem id: conta no total, nunca em voo
        assert_eq!((t.total, t.open()), (4, 0));
    }

    #[test]
    fn open_ids_are_capped_and_the_oldest_leaves_first() {
        let mut t = ToolCalls::default();
        for i in 0..(OPEN_CAP as u64 + 10) {
            t.called(Some(i), None);
        }
        assert_eq!(t.open(), OPEN_CAP as u64);
        t.answered(0); // ja saiu pelo teto
        assert_eq!(t.open(), OPEN_CAP as u64);
        t.answered(OPEN_CAP as u64 + 9);
        assert_eq!(t.open(), OPEN_CAP as u64 - 1);
    }

    #[test]
    fn per_min_averages_the_last_five_minute_buckets() {
        let mut t = ToolCalls::default();
        // 10 por minuto nos minutos 0..=4; no minuto 5 + 30 s a janela e
        // [1, 5.5): minutos 1..=4 (40) mais 5 do atual, em 4,5 min.
        for m in 0..5 {
            for _ in 0..10 {
                t.called(None, Some(T0 + m * MINUTE_MS + 1_000));
            }
        }
        for _ in 0..5 {
            t.called(None, Some(T0 + 5 * MINUTE_MS + 1_000));
        }
        assert_eq!(t.per_min(T0 + 5 * MINUTE_MS + 30_000), 10.0);
        // No fim do minuto 4 a janela [0, 5) cobre os 50 em 5 min.
        let mut u = ToolCalls::default();
        for m in 0..5 {
            for _ in 0..10 {
                u.called(None, Some(T0 + m * MINUTE_MS));
            }
        }
        assert_eq!(u.per_min(T0 + 5 * MINUTE_MS - 1), 10.0);
        // Dez minutos depois, nada na janela.
        assert_eq!(u.per_min(T0 + 15 * MINUTE_MS), 0.0);
        // Linha velha (fora da janela do balde) nao reaproveita o balde novo.
        u.called(None, Some(T0 + 10 * MINUTE_MS));
        u.called(None, Some(T0));
        assert_eq!(u.per_min(T0 + 10 * MINUTE_MS + 30_000), 0.2);
    }

    fn canon(json: &str) -> String {
        let mut d = serde_json::Deserializer::from_str(json);
        Canon::TOP.deserialize(&mut d).unwrap().text.s
    }

    fn args_of(json: &str) -> Args {
        let mut d = serde_json::Deserializer::from_str(json);
        args(&mut d).unwrap()
    }

    /// Esperado: `norm_args(json.loads(x))` do `contexto.py`.
    #[test]
    fn canonical_form_matches_norm_args() {
        let cases = [
            (
                r#"{"b":1,"a":[1,2.5,null,true,false],"description":"x","timeout":5,"run_in_background":true}"#,
                r#"{"a": [1, 2.5, null, true, false], "b": 1}"#.to_string(),
            ),
            (
                r#"{"z":{"description":"keep","a":"é\n\t\"\\\u0001\u007f 😀"},"y":[],"x":{}}"#,
                "{\"x\": {}, \"y\": [], \"z\": {\"a\": \"\u{e9}\\n\\t\\\"\\\\\\u0001\u{7f}\u{2028}\u{1f600}\", \"description\": \"keep\"}}".to_string(),
            ),
            (
                "[1e16,1e-5,0.0001,100.0,-0.0,1.5e300,123456789012345.6,1e22,0.1,5e-324]",
                "[1e+16, 1e-05, 0.0001, 100.0, -0.0, 1.5e+300, 123456789012345.6, 1e+22, 0.1, 5e-324]".to_string(),
            ),
            (r#"{"a":1,"b":0,"a":2}"#, r#"{"a": 2, "b": 0}"#.to_string()),
            (
                r#""{\"command\": [\"echo\"]}""#,
                r#""{\"command\": [\"echo\"]}""#.to_string(),
            ),
            (
                r#"{"ké":"v","K":"w","_":"u"}"#,
                "{\"K\": \"w\", \"_\": \"u\", \"k\u{e9}\": \"v\"}".to_string(),
            ),
        ];
        for (json, want) in cases {
            assert_eq!(canon(json), want, "{json}");
        }
    }

    #[test]
    fn canonical_form_is_cut_at_400_characters_not_bytes() {
        let long = format!(r#"{{"content":"{}"}}"#, "\u{e9}".repeat(500));
        let c = canon(&long);
        assert_eq!(c.chars().count(), ARGS_MAX_CHARS);
        assert!(c.starts_with("{\"content\": \"\u{e9}"), "{c}");
        assert!(
            c.len() > ARGS_MAX_CHARS,
            "corte em bytes deixaria 400 bytes"
        );
        // Iguais nos 400 primeiros caracteres: a mesma chamada.
        let a = format!(r#"{{"content":"{}A"}}"#, "\u{e9}".repeat(450));
        let b = format!(r#"{{"content":"{}B"}}"#, "\u{e9}".repeat(450));
        assert_eq!(args_of(&a), args_of(&b));
        // Diferentes no caractere 400 (o ultimo que fica): outra chamada.
        let head = "x".repeat(ARGS_MAX_CHARS - 14);
        let a = format!(r#"{{"content":"{head}A"}}"#);
        let b = format!(r#"{{"content":"{head}B"}}"#);
        assert_eq!(canon(&a).chars().count(), ARGS_MAX_CHARS);
        assert_ne!(args_of(&a), args_of(&b));
    }

    #[test]
    fn falsy_arguments_take_the_or_fallback() {
        for json in ["{}", "\"\"", "0", "0.0", "null", "false", "[]"] {
            assert_eq!(args_of(json), Args::Falsy, "{json}");
        }
        assert_eq!(Args::Falsy.or(Args::Hash(7)), Args::Hash(7));
        assert_eq!(Args::Hash(1).or(Args::Hash(7)), Args::Hash(1));
        // `{"description": ...}` e verdadeiro, mas vira `{}` depois do filtro:
        // a mesma chave de uma chamada sem argumentos.
        let n = Some(id_hash(b"Bash"));
        let only_volatile = args_of(r#"{"description":"x"}"#);
        assert_ne!(only_volatile, Args::Falsy);
        assert_eq!(
            call_key(n, only_volatile, Empty::Object),
            call_key(n, Args::Falsy, Empty::Object)
        );
        // Sem nome e com nome vazio: `"?"`; argumento nao lido: fora.
        assert_eq!(
            call_key(None, Args::Falsy, Empty::Text),
            call_key(Some(id_hash(b"?")), Args::Falsy, Empty::Text)
        );
        assert_ne!(
            call_key(n, Args::Falsy, Empty::Object),
            call_key(n, Args::Falsy, Empty::Text)
        );
        assert_eq!(call_key(n, Args::Unread, Empty::Object), None);
    }

    #[test]
    fn skipping_restores_the_flag_even_after_a_panic() {
        assert!(!SKIP.with(Cell::get));
        let r = std::panic::catch_unwind(|| skipping(|| panic!("no meio da linha")));
        assert!(r.is_err());
        assert!(
            !SKIP.with(Cell::get),
            "SKIP preso pularia os argumentos para sempre"
        );
        assert_eq!(skipping(|| args_of(r#"{"a":1}"#)), Args::Unread);
        assert_ne!(args_of(r#"{"a":1}"#), Args::Unread);
    }

    #[test]
    fn loops_follow_the_script_over_the_last_thirty_calls() {
        let mut t = ToolCalls::default();
        assert_eq!(
            t.loops(),
            Loops {
                max_streak: 1,
                repeated_max: 0
            },
            "sem chamadas: o best = 1 do script"
        );
        for k in [1, 2, 2, 3, 1, 1, 1, 4, 1] {
            t.repeated(k);
        }
        assert_eq!(
            t.loops(),
            Loops {
                max_streak: 3,
                repeated_max: 5
            }
        );
        assert_eq!(t.loops().repeats(), Some(5));
        // 30 chamadas novas, todas diferentes: as antigas saem da janela.
        for k in 100..130 {
            t.repeated(k);
        }
        assert_eq!(
            t.loops(),
            Loops {
                max_streak: 1,
                repeated_max: 1
            }
        );
        assert_eq!(t.loops().repeats(), None);
        // A janela gira: 3 iguais nas ultimas 30, nao seguidas.
        for k in [7, 8, 7, 9, 7] {
            t.repeated(k);
        }
        assert_eq!(
            t.loops(),
            Loops {
                max_streak: 1,
                repeated_max: 3
            }
        );
        assert_eq!(t.loops().repeats(), Some(3));
        for k in 200..226 {
            t.repeated(k);
        }
        // O primeiro 7 saiu (sobram 2): nao e mais loop.
        assert_eq!(t.loops().repeated_max, 2);
    }
}
