//! Metricas por processo e soma por sessao.

use crate::adapters::AgentRole;
use crate::overlay::SessionOverlay;
use serde::{Deserialize, Serialize};

/// Stall (% do intervalo esperando E/S de bloco) a partir do qual a sessao pode
/// ser io_bound. 20 %: um quinto do tempo parado em disco ja domina a latencia.
pub const IO_BOUND_STALL_PCT: f64 = 20.0;
/// CPU (% de um nucleo, soma da sessao) abaixo da qual stall alto vira io_bound.
/// Acima disso a sessao esta trabalhando, nao esperando.
pub const IO_BOUND_MAX_CPU_PCT: f64 = 10.0;
/// No fallback (sem delayacct): threads em D na sessao que contam como stall alto.
pub const IO_BOUND_D_THREADS: u32 = 1;

/// `true` se a sessao esta esperando disco: stall alto e CPU baixa.
pub fn io_bound(cpu_pct: f64, stall_pct: Option<f64>, threads_d: Option<u32>) -> bool {
    let stalled = match (stall_pct, threads_d) {
        (Some(s), _) => s >= IO_BOUND_STALL_PCT,
        (None, Some(d)) => d >= IO_BOUND_D_THREADS,
        (None, None) => false,
    };
    stalled && cpu_pct < IO_BOUND_MAX_CPU_PCT
}

/// Metrica somada por sessao que pode faltar em algum processo. Aparece em
/// [`Session::partial`] quando a soma deixou de fora ao menos um processo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    CpuPct,
    PssBytes,
    SwapPssBytes,
    ReadBps,
    WriteBps,
    StallPct,
}

/// Um processo de uma sessao. Campos `None`: ilegivel (outro usuario) ou sem
/// amostra anterior para delta (primeiro tick, processo novo, pid reusado).
/// Ausente nunca vira 0: 0 e uma medida.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessMetrics {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub state: char,
    pub uid: u32,
    pub threads: u32,
    /// % de um nucleo; `None` sem amostra anterior.
    pub cpu_pct: Option<f64>,
    /// RSS do processo; mostrado, nunca somado (conta paginas compartilhadas varias vezes).
    /// `None` sem `VmRSS` no status (zumbi, thread de kernel).
    pub rss_bytes: Option<u64>,
    pub pss_bytes: Option<u64>,
    pub swap_bytes: Option<u64>,
    pub swap_pss_bytes: Option<u64>,
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    pub rchar_bps: Option<f64>,
    pub wchar_bps: Option<f64>,
    pub stall_pct: Option<f64>,
    pub threads_d: Option<u32>,
    pub voluntary_ctxt_switches: u64,
    pub nonvoluntary_ctxt_switches: u64,
}

/// Uma sessao: processo agente + descendentes que nao sao de agente mais profundo.
///
/// Somas sao das medidas presentes: `None` quando nenhum processo tem o valor,
/// e a metrica entra em `partial` quando algum processo ficou de fora da soma.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Id do adapter (`claude`, `codex`, `pi`, `herdr`, ou o comm extra).
    pub agent: String,
    pub role: AgentRole,
    /// pid do processo agente.
    pub pid: u32,
    /// Kernel process identity sampled from stat; absent in older snapshots.
    #[serde(default)]
    pub starttime: Option<u64>,
    pub cmdline: Option<String>,
    pub cwd: Option<String>,
    /// Numero de processos na sessao, agente incluso.
    pub procs: usize,
    /// Soma, em % de um nucleo.
    pub cpu_pct: Option<f64>,
    /// `cpu_pct` como fatia da maquina (0..=100).
    pub cpu_share_pct: Option<f64>,
    /// Somas de PSS/SwapPss: honestas porque paginas compartilhadas sao divididas.
    pub pss_bytes: Option<u64>,
    pub swap_pss_bytes: Option<u64>,
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    /// Maior stall entre os processos (delayacct); `None` no fallback.
    pub stall_pct: Option<f64>,
    /// Threads em D na sessao (fallback); `None` com delayacct.
    pub threads_d: Option<u32>,
    /// Exige CPU conhecida: sem amostra de CPU nao da para dizer que esta parada.
    pub io_bound: bool,
    /// Metricas cuja soma nao cobre todos os processos, em ordem de [`Metric`].
    pub partial: Vec<Metric>,
    pub overlay: Option<SessionOverlay>,
    pub processes: Vec<ProcessMetrics>,
}

/// Soma das medidas presentes e se alguma faltou. `None` se nenhuma existe.
fn sum_known<T: Copy + std::iter::Sum<T>>(
    processes: &[ProcessMetrics],
    f: impl Fn(&ProcessMetrics) -> Option<T>,
) -> (Option<T>, bool) {
    let known = processes.iter().filter_map(&f).count();
    let sum = (known > 0).then(|| processes.iter().filter_map(&f).sum());
    (sum, known < processes.len())
}

impl Session {
    /// Soma as metricas dos processos. `cores` converte para fatia da maquina.
    pub fn aggregate(
        agent: String,
        role: AgentRole,
        pid: u32,
        cores: u32,
        mut processes: Vec<ProcessMetrics>,
    ) -> Self {
        processes.sort_by_key(|p| (p.pid != pid, p.pid));
        let mut partial = Vec::new();
        let mut track = |m: Metric, missing: bool| {
            if missing {
                partial.push(m);
            }
        };
        let (cpu_pct, miss) = sum_known(&processes, |p| p.cpu_pct);
        track(Metric::CpuPct, miss);
        let (pss_bytes, miss) = sum_known(&processes, |p| p.pss_bytes);
        track(Metric::PssBytes, miss);
        let (swap_pss_bytes, miss) = sum_known(&processes, |p| p.swap_pss_bytes);
        track(Metric::SwapPssBytes, miss);
        let (read_bps, miss) = sum_known(&processes, |p| p.read_bps);
        track(Metric::ReadBps, miss);
        let (write_bps, miss) = sum_known(&processes, |p| p.write_bps);
        track(Metric::WriteBps, miss);
        // No fallback (threads_d presente) stall_pct ausente e o normal, nao falta.
        track(
            Metric::StallPct,
            processes
                .iter()
                .any(|p| p.stall_pct.is_none() && p.threads_d.is_none()),
        );
        let stall_pct = processes
            .iter()
            .filter_map(|p| p.stall_pct)
            .reduce(f64::max);
        let threads_d = processes
            .iter()
            .filter_map(|p| p.threads_d)
            .reduce(|a, b| a + b);
        Session {
            agent,
            role,
            pid,
            starttime: None,
            cmdline: None,
            cwd: None,
            procs: processes.len(),
            cpu_pct,
            cpu_share_pct: cpu_pct.map(|c| c / f64::from(cores.max(1))),
            pss_bytes,
            swap_pss_bytes,
            read_bps,
            write_bps,
            stall_pct,
            threads_d,
            io_bound: cpu_pct.is_some_and(|c| io_bound(c, stall_pct, threads_d)),
            partial,
            overlay: None,
            processes,
        }
    }

    /// Chave de ordenacao: memoria residente proporcional + swap proporcional.
    /// Ausente conta 0 aqui, so para ordenar; o valor publicado continua `None`.
    pub fn memory_cost(&self) -> u64 {
        self.pss_bytes.unwrap_or(0) + self.swap_pss_bytes.unwrap_or(0)
    }
}
