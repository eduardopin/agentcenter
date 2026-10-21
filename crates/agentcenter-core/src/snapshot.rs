//! Snapshot imutavel de um tick: o que a CLI imprime e a GUI recebe pelo canal.

use crate::machine::{LoadAvg, Psi};
use crate::session::Session;
use crate::summary::Summary;
use serde::{Deserialize, Serialize};

/// Versao do formato JSON. Sobe quando um campo muda de sentido ou sai.
///
/// - 2: `processes[].rss_bytes` passa a ser `null` quando o kernel nao da
///   `VmRSS` (antes saia 0); `pss_bytes`/`swap_pss_bytes`/`swap_bytes` e
///   `threads_d` passam a ser os da ultima releitura (cadencia de smaps:
///   todo tick na sessao selecionada, ate 5 s nas demais se a memoria mexeu,
///   ate 60 s se nao mexeu), nao mais os do tick. Campos novos de maquina:
///   memoria, swap, carga e PSI.
/// - 3: `uid` e `voluntary_ctxt_switches`/`nonvoluntary_ctxt_switches` passam
///   a ser os do ultimo `status` lido (varredura completa a cada 5 s, todo tick
///   na sessao selecionada), nao mais os do tick; `rss_bytes` e `threads` saem
///   do `stat` do tick. Sem mudanca de memoria, PSS e swap sao relidos ate
///   300 s depois (eram 60 s).
/// - 4: campo novo `summary` (contagens e somas do tick, ver [`Summary`]) e
///   campos reservados em `overlay` (`tool_calls`, `tool_calls_open`,
///   `tool_calls_per_min`, `subagents_live`, `subagents_total`), `null` por
///   enquanto. JSON sem `summary` (v3) le com o resumo zerado. Desde o C14
///   o overlay de transcripts preenche os `tool_calls*`, e desde o C15 os
///   `subagents_*` (o sentido e o reservado; nao muda a versao).
/// - 5: cockpit por sessao (C21): `overlay.provider`, `overlay.cockpit` (as
///   contas e o veredito do `contexto.py`, ver [`crate::cockpit::Cockpit`]) e
///   `overlay.cockpit_pending`; no resumo, `summary.cockpit` (sessoes por
///   recomendacao, soma = `sessions`), `cockpit_pending`,
///   `sessions_burn_weighted`, `cache_hit` e `ctx_pct_max`. JSON v4 le com
///   esses campos `null`, zerados ou `false`.
///   Desde o C23, `overlay.cockpit.max_streak` e `overlay.cockpit.repeated_max`
///   (sinal de loop, ver [`crate::cockpit::Loops`]) e a recomendacao LOOP
///   calculada; campo novo sem mudar sentido de outro, entao a versao fica.
///   JSON sem eles le com 0 (sem sinal).
pub const SCHEMA_VERSION: u32 = 5;

/// De onde vem o stall de E/S.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoStallSource {
    /// `kernel.task_delayacct=1`: delta do campo 42 de `stat` (`stall_pct`).
    Delayacct,
    /// Desligado: contagem de threads em estado D nas arvores de sessao (`threads_d`).
    DState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub cores: u32,
    /// Uso da maquina inteira (0..=100), de `/proc/stat`; `None` no primeiro tick.
    pub cpu_pct: Option<f64>,
    pub io_stall_source: IoStallSource,
    /// Processos lidos neste tick.
    pub processes: usize,
    /// Processos que pertencem a alguma sessao.
    pub session_processes: usize,
    /// De `/proc/meminfo`, em bytes; `None` se o arquivo ou a linha faltar.
    pub mem_total_bytes: Option<u64>,
    pub mem_available_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub swap_free_bytes: Option<u64>,
    /// `/proc/loadavg`.
    pub load_avg: Option<LoadAvg>,
    /// `/proc/pressure/{cpu,memory,io}`; `None` em kernel sem PSI.
    pub psi: Option<Psi>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    /// Instante do tick, ms desde a epoca Unix.
    pub taken_at_ms: u64,
    /// Intervalo coberto pelos deltas, em ms (0 no primeiro tick: sem taxas).
    pub interval_ms: u64,
    pub machine: Machine,
    /// Ordenadas por `pss_bytes + swap_pss_bytes`, maior primeiro.
    pub sessions: Vec<Session>,
    /// Calculado de `sessions` no mesmo tick, depois dos overlays.
    #[serde(default)]
    pub summary: Summary,
}
