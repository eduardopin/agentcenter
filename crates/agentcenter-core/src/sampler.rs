//! Um tick: le `/proc`, atribui processos a sessoes, calcula deltas contra o
//! tick anterior e devolve um [`Snapshot`].
//!
//! Cadencia (ARCHITECTURE, "Sampling"): lista de processos (readdir), e `stat`
//! e `io` dos processos de sessao todo tick. O RSS sai do campo 24 do `stat`.
//! O `stat` de todos os pids (varredura completa) so a cada `smaps_idle_every`
//! (5 s); nos ticks entre uma e outra le-se o `stat` so dos pids novos (que nao
//! estavam na listagem anterior) e dos que eram de sessao no tick anterior, e os
//! demais repetem o ultimo `stat` lido. A varredura completa pega exec (troca de
//! comm), pid reusado fora de sessao e reparenting de quem nao e de sessao.
//!
//! Sessao quieta: na ultima leitura todos os seus processos tinham leitura
//! anterior, a CPU somada deles na janela desde a leitura anterior ficou abaixo
//! de `QUIET_MAX_CPU_PCT` (1 % de um nucleo), nenhum andou em
//! `read_bytes`/`write_bytes` nem `delayacct_blkio_ticks`, e nenhum estava em
//! `D` (processo ou thread). O `D` entra de proposito: quem esta preso em
//! swap-in tem CPU ~0 e `read_bytes` 0, mas esta parado no disco. "Andar" e o
//! contador mudar, em qualquer direcao.
//! Sessao quieta, nao selecionada e sem processo novo nao e lida fora da
//! varredura completa: o `stat` fica o do cache e cada processo republica as
//! ultimas metricas, sem abrir `io`, `status`, `smaps_rollup` nem `task/`. Na
//! varredura completa (ou quando aparece nela um pid que nao estava no tick
//! anterior) a sessao e relida inteira e a regra vale de novo. Alinhar com a
//! varredura e um desvio do "espalhar por pid" do card C6c: aquele tick ja le o
//! `stat` de todos os pids, e espalhar as releituras em outros ticks leria o
//! `stat` desses pids duas vezes a cada 5 s. Uma sessao que acorda mostra a
//! media da ultima janela (abaixo de 1 %) por ate 5 s; as taxas sao calculadas
//! sobre o intervalo real desde a ultima leitura do processo (`cpu.total`
//! daquele momento), entao a primeira leitura depois de quieta e a media do
//! periodo, sem pico falso. As metricas republicadas so sao copiadas para as
//! sessoes que terminam o tick quietas: com tudo ativo, nada e copiado.
//!
//! `cmdline` e `cwd` do agente: lidos quando o agente aparece (pid novo ou
//! reusado), na varredura completa e todo tick na selecionada; nas demais o
//! valor fica ate 5 s defasado.
//!
//! O `status` (uid, `VmSwap`, ctx switches) e lido na varredura completa, no
//! processo novo (ou pid reusado) e todo tick na sessao selecionada; nas demais
//! os valores ficam ate 5 s defasados.
//!
//! `smaps_rollup` custa caro ao kernel (~5 ms num processo de 400 MB: percorre
//! as tabelas de pagina) e a varredura de `task/` cresce com as threads, entao os
//! dois so sao relidos todo tick para a sessao selecionada. Nas demais: a
//! varredura de `task/` a cada `smaps_idle_every` (5 s); o `smaps_rollup` a cada
//! `smaps_idle_every` se o RSS (do `stat`) ou o `VmSwap` (do ultimo `status`)
//! andaram mais de 1/256 desde a ultima leitura, e no maximo a cada
//! `smaps_max_age` (300 s) se nao andaram. Processo novo e lido na hora.
//!
//! [`Sampler::spawn`] roda os ticks numa thread e entrega cada [`Frame`] por
//! `std::sync::mpsc`: a GUI nunca toca `/proc`.

use crate::adapters::{AgentRole, Registry};
use crate::history::History;
use crate::machine::read_machine;
use crate::overlay::transcript::READ_BUDGET;
use crate::overlay::{self, Overlay};
use crate::procfs::io::{IoCounters, read_io};
use crate::procfs::smaps::{SmapsRollup, parse_smaps_rollup};
use crate::procfs::stat::{Stat, parse_stat};
use crate::procfs::status::{Status, parse_status};
use crate::procfs::system::{CpuTotals, delayacct_enabled, read_cpu_totals};
use crate::procfs::task::count_d_threads;
use crate::procfs::{ProcRoot, ReadError};
use crate::session::{ProcessMetrics, Session};
use crate::snapshot::{IoStallSource, Machine, SCHEMA_VERSION, Snapshot};
use crate::summary::Summary;
use crate::tree::Attribution;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Ticks de CPU por segundo (USER_HZ). E 100 em todas as arquiteturas Linux
/// suportadas; evita depender de libc so para `sysconf(_SC_CLK_TCK)`.
pub const USER_HZ: f64 = 100.0;

/// Fracao do RSS (1/256, ~0,4 %) que o RSS ou o `VmSwap` precisam andar
/// para o `smaps_rollup` de uma sessao nao selecionada ser relido antes de
/// `smaps_max_age`. Medido em 2026-10-15 (278 processos de sessao, 5 s): 3
/// andaram isso, e reler so eles custou 9 ms dos 184 ms de reler todos.
const SMAPS_MOVED_SHIFT: u32 = 8;

/// CPU (% de um nucleo, soma dos processos da sessao na janela desde a leitura
/// anterior) abaixo da qual a sessao ainda conta como quieta. Decisao do dono
/// (2026-10-16): um agente parado ainda acorda timers e faz poll, e exigir
/// contador de CPU igual deixava quase toda sessao acordada. Disco e stall
/// continuam exigindo contador igual.
const QUIET_MAX_CPU_PCT: f64 = 1.0;

/// Intervalos do sampler. Os de smaps sao convertidos em ticks
/// (arredondando para cima), entao a cadencia e deterministica em fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplerConfig {
    /// Entre ticks em [`Sampler::spawn`]; base da conversao para ticks.
    pub interval: Duration,
    /// Releitura de smaps (se a memoria andou), de `task/` e de `status` fora da
    /// selecionada; tambem o periodo da varredura completa de `stat`.
    pub smaps_idle_every: Duration,
    /// Releitura de smaps mesmo sem a memoria andar (PSS muda quando quem
    /// compartilha as paginas muda). 300 s: com a memoria parada o PSS quase nao
    /// anda, e reler os ~290 processos de sessao custava 185 ms por passada.
    pub smaps_max_age: Duration,
    /// Bytes de transcript lidos por tick, somados todos os arquivos (overlay
    /// de transcripts). O padrao, [`READ_BUDGET`], segura o tick do monitor; o
    /// comando de um tiro (`snapshot`) usa [`READ_ALL`] para que os tokens e o
    /// cockpit saiam ja no segundo tick. [`READ_ALL`] tambem tira o teto da
    /// primeira leitura dos subagentes, para que `subagents_live` saia
    /// completo; qualquer outro valor deixa o teto proprio deles.
    pub transcript_read_budget: u64,
}

/// Sem teto de leitura de transcript por tick.
pub const READ_ALL: u64 = u64::MAX;

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            smaps_idle_every: Duration::from_secs(5),
            smaps_max_age: Duration::from_secs(300),
            transcript_read_budget: READ_BUDGET,
        }
    }
}

/// Quantos ticks cabem em `d`, para cima, no minimo 1.
fn ticks_in(d: Duration, interval: Duration) -> u64 {
    let i = interval.as_nanos().max(1);
    d.as_nanos().div_ceil(i).max(1) as u64
}

/// Ultima leitura cara de um processo, reaproveitada entre releituras.
#[derive(Debug, Clone, Copy)]
struct MemCache {
    smaps: Option<SmapsRollup>,
    /// Tick da leitura de smaps, e o RSS e o `VmSwap` naquele momento.
    smaps_at: u64,
    rss: u64,
    swap: u64,
    threads_d: u32,
    d_at: u64,
}

/// Contadores guardados de uma leitura do processo para a proxima.
#[derive(Debug, Clone)]
struct Prev {
    starttime: u64,
    cpu_ticks: u64,
    blkio_ticks: u64,
    io: Option<IoCounters>,
    mem: MemCache,
    /// Ultimo `status` lido (ver o topo do modulo para quando e relido).
    status: Status,
    /// `cpu.total` da maquina no tick da leitura: base do intervalo das taxas.
    cpu_total: u64,
    /// Tick da leitura.
    read_at: u64,
    /// pid do agente da sessao do processo naquela leitura.
    agent: u32,
    /// Metricas publicadas naquela leitura, guardadas so se a sessao terminou o
    /// tick quieta: republicadas enquanto ela ficar quieta e sem leitura. A
    /// sessao ativa nao paga a copia.
    metrics: Option<ProcessMetrics>,
}

/// cmdline e cwd do agente de uma sessao, relidos na varredura completa, na
/// selecionada e quando o agente aparece (pid novo ou reusado).
#[derive(Debug, Clone)]
struct AgentInfo {
    starttime: u64,
    cmdline: Option<String>,
    cwd: Option<String>,
}

/// O que vale para todos os processos de um tick.
#[derive(Debug, Clone, Copy)]
struct TickCtx {
    delayacct: bool,
    /// Tick de varredura completa: `status` relido em todos os de sessao.
    full: bool,
    /// `cpu.total` e nucleos da maquina neste tick.
    cpu_total: u64,
    cores: u32,
}

/// Amostrador com estado: o primeiro tick nao tem taxas; os seguintes sao deltas.
pub struct Sampler {
    registry: Registry,
    overlays: Vec<Box<dyn Overlay>>,
    prev: HashMap<u32, Prev>,
    prev_cpu: Option<CpuTotals>,
    buf: String,
    config: SamplerConfig,
    /// Ticks feitos ate agora (o proximo tick e o de numero `tick_no`).
    tick_no: u64,
    idle_every: u64,
    max_age: u64,
    /// pid do agente da sessao selecionada (detalhe aberto na GUI).
    selected: Option<u32>,
    history: History,
    /// Ultimo `stat` lido de cada pid vivo (de sessao ou nao).
    stats: HashMap<u32, Stat>,
    /// Tick da ultima varredura completa de `stat`.
    full_scan_at: u64,
    /// pids dos agentes das sessoes quietas (ver o topo do modulo).
    quiet: HashSet<u32>,
    /// Por pid de agente das sessoes do ultimo tick.
    agents: HashMap<u32, AgentInfo>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new(Registry::from_env())
    }
}

impl Sampler {
    pub fn new(registry: Registry) -> Self {
        Self::with_config(registry, SamplerConfig::default())
    }

    pub fn with_config(registry: Registry, config: SamplerConfig) -> Self {
        let mut s = Self {
            registry,
            overlays: overlay::registry(),
            prev: HashMap::new(),
            prev_cpu: None,
            buf: String::with_capacity(4096),
            config,
            tick_no: 0,
            idle_every: 1,
            max_age: 1,
            selected: None,
            history: History::default(),
            stats: HashMap::new(),
            full_scan_at: 0,
            quiet: HashSet::new(),
            agents: HashMap::new(),
        };
        s.set_config(config);
        s
    }

    pub fn config(&self) -> SamplerConfig {
        self.config
    }

    /// Troca os intervalos; vale a partir do proximo tick.
    pub fn set_config(&mut self, config: SamplerConfig) {
        self.config = config;
        for ov in &self.overlays {
            ov.configure(&config);
        }
        self.idle_every = ticks_in(config.smaps_idle_every, config.interval);
        self.max_age = ticks_in(config.smaps_max_age, config.interval).max(self.idle_every);
    }

    pub fn set_interval(&mut self, interval: Duration) {
        self.set_config(SamplerConfig {
            interval,
            ..self.config
        });
    }

    /// Marca a sessao (pid do agente) cujo smaps e `task/` sao lidos todo tick.
    /// `None` desmarca.
    pub fn select(&mut self, agent_pid: Option<u32>) {
        self.selected = agent_pid;
    }

    pub fn selected(&self) -> Option<u32> {
        self.selected
    }

    /// Series das sessoes vivas no ultimo tick.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Roda os ticks numa thread propria, um a cada `config.interval`, e entrega
    /// cada resultado pelo canal do [`SamplerHandle`]. O canal tem uma vaga:
    /// consumidor atrasado segura o sampler (nada se acumula na memoria).
    /// `with_history` copia o [`History`] em cada frame (para a GUI; a CLI nao precisa).
    pub fn spawn(self, root: ProcRoot, with_history: bool) -> SamplerHandle {
        let (tx, rx) = mpsc::sync_channel(1);
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("agentcenter-sampler".into())
            .spawn(move || run(self, root, with_history, tx, ctl_rx))
            .expect("spawn sampler thread");
        SamplerHandle {
            rx: Some(rx),
            ctl: Some(ctl_tx),
            thread: Some(thread),
        }
    }

    /// Le `root` e produz o snapshot deste instante.
    pub fn tick(&mut self, root: &ProcRoot) -> Result<Snapshot, ReadError> {
        let cpu = read_cpu_totals(root, &mut self.buf)?;
        let delayacct = delayacct_enabled(root, &mut self.buf);
        let machine = read_machine(root, &mut self.buf);

        // 1. stat: arvore e deteccao. Quem some da listagem ja fica de fora.
        // Varredura completa a cada `idle_every`; entre elas, so pids novos e
        // de sessao que nao estava quieta (ver o topo do modulo).
        let pids = root.pids()?;
        let full = self.stats.is_empty() || self.tick_no - self.full_scan_at >= self.idle_every;
        if full {
            self.full_scan_at = self.tick_no;
        }
        let mut stats: HashMap<u32, Stat> = HashMap::with_capacity(pids.len());
        // pids de sessao quieta com o `stat` do cache: relidos no passo 3 se a
        // sessao for lida neste tick.
        let mut stale: HashSet<u32> = HashSet::new();
        for pid in pids {
            let cached = match self.prev.get(&pid) {
                None => true,
                Some(p) => self.quiet.contains(&p.agent) && self.selected != Some(p.agent),
            };
            if !full
                && cached
                && let Some(st) = self.stats.remove(&pid)
            {
                if self.prev.contains_key(&pid) {
                    stale.insert(pid);
                }
                stats.insert(pid, st);
                continue;
            }
            if root.read_pid(pid, "stat", &mut self.buf).is_err() {
                continue;
            }
            if let Ok(st) = parse_stat(&self.buf) {
                stats.insert(pid, st);
            }
        }
        let parents: HashMap<u32, u32> = stats.iter().map(|(&p, s)| (p, s.ppid)).collect();
        let mut agent_of: HashMap<u32, (String, AgentRole)> = HashMap::new();
        let registry = &self.registry;
        let attribution = Attribution::build(&parents, |pid| {
            let comm = stats[&pid].comm.as_str();
            let mut exe = || exe_basename(root, pid);
            match registry.detect(comm, &mut exe) {
                Some(a) => {
                    agent_of.insert(pid, (a.id().to_string(), a.role()));
                    true
                }
                None => false,
            }
        });

        // 2. Intervalo, medido em ticks de CPU da maquina (deterministico em fixture).
        let cores = cpu.cores;
        let (d_total, d_idle) = match self.prev_cpu {
            Some(p) if cpu.total > p.total => {
                (cpu.total - p.total, cpu.idle.saturating_sub(p.idle))
            }
            _ => (0, 0),
        };
        let ticks_per_core = d_total as f64 / f64::from(cores);
        let secs = ticks_per_core / USER_HZ;
        let machine_cpu =
            (d_total > 0).then(|| (d_total.saturating_sub(d_idle)) as f64 / d_total as f64 * 100.0);

        // 3. Detalhe so dos processos de sessao, sessao a sessao: a quieta so e
        // lida na varredura completa, se selecionada ou se tem processo novo.
        let ctx = TickCtx {
            delayacct,
            full,
            cpu_total: cpu.total,
            cores,
        };
        let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&pid, &agent) in &attribution.owner {
            members.entry(agent).or_default().push(pid);
        }
        let mut by_agent: HashMap<u32, Vec<ProcessMetrics>> = HashMap::with_capacity(members.len());
        let mut next_prev: HashMap<u32, Prev> = HashMap::with_capacity(attribution.owner.len());
        let mut quiet: HashSet<u32> = HashSet::with_capacity(self.quiet.len());
        for (agent, pids) in members {
            let selected = self.selected == Some(agent);
            // Processo novo (ou que veio de outra sessao) acorda a sessao.
            let read = full
                || selected
                || !self.quiet.contains(&agent)
                || pids.iter().any(|p| {
                    self.prev
                        .get(p)
                        .is_none_or(|pr| pr.agent != agent || pr.metrics.is_none())
                });
            let procs = by_agent.entry(agent).or_default();
            if !read {
                for pid in pids {
                    if let Some(prev) = self.prev.remove(&pid) {
                        procs.extend(prev.metrics.clone());
                        next_prev.insert(pid, prev);
                    }
                }
                quiet.insert(agent);
                continue;
            }
            // Quieta se nenhum processo andou em disco nem stall e a CPU somada
            // ficou abaixo de `QUIET_MAX_CPU_PCT`.
            let mut still = true;
            let mut cpu = 0.0;
            let mut read_pids: Vec<u32> = Vec::with_capacity(pids.len());
            for pid in pids {
                if stale.contains(&pid) {
                    let fresh = root
                        .read_pid(pid, "stat", &mut self.buf)
                        .ok()
                        .and_then(|()| parse_stat(&self.buf).ok());
                    match fresh {
                        Some(st) => stats.insert(pid, st),
                        None => {
                            stats.remove(&pid); // sumiu
                            continue;
                        }
                    };
                }
                let st = &stats[&pid];
                let Some((m, prev, quiet_cpu)) = self.read_process(root, st, agent, selected, ctx)
                else {
                    continue; // sumiu no meio da leitura
                };
                match quiet_cpu {
                    Some(c) => cpu += c,
                    None => still = false,
                }
                next_prev.insert(pid, prev);
                read_pids.push(pid);
                procs.push(m);
            }
            if still && cpu < QUIET_MAX_CPU_PCT {
                quiet.insert(agent);
                for (pid, m) in read_pids.iter().zip(procs.iter()) {
                    if let Some(p) = next_prev.get_mut(pid) {
                        p.metrics = Some(m.clone());
                    }
                }
            }
        }

        let mut sessions: Vec<Session> = Vec::with_capacity(by_agent.len());
        let mut agents: HashMap<u32, AgentInfo> = HashMap::with_capacity(by_agent.len());
        for (agent_pid, procs) in by_agent {
            // Agente que sumiu mas deixou filhos: os filhos ficam sem sessao neste tick.
            if !procs.iter().any(|p| p.pid == agent_pid) {
                continue;
            }
            let (id, role) = agent_of[&agent_pid].clone();
            let mut s = Session::aggregate(id, role, agent_pid, cores, procs);
            s.starttime = stats.get(&agent_pid).map(|st| st.starttime);
            let starttime = s.starttime.unwrap_or(0);
            let info = match self.agents.remove(&agent_pid) {
                Some(i)
                    if !full && self.selected != Some(agent_pid) && i.starttime == starttime =>
                {
                    i
                }
                _ => AgentInfo {
                    starttime,
                    cmdline: read_cmdline(root, agent_pid, &mut self.buf),
                    cwd: root
                        .read_link_pid(agent_pid, "cwd")
                        .map(|p| p.to_string_lossy().into_owned()),
                },
            };
            s.cmdline = info.cmdline.clone();
            s.cwd = info.cwd.clone();
            agents.insert(agent_pid, info);
            sessions.push(s);
        }
        self.agents = agents;
        sessions.sort_by(|a, b| {
            b.memory_cost()
                .cmp(&a.memory_cost())
                .then(a.pid.cmp(&b.pid))
        });
        overlay::apply(&self.overlays, &mut sessions);
        self.history
            .record(&sessions, |pid| stats.get(&pid).map_or(0, |s| s.starttime));

        self.prev = next_prev;
        self.quiet = quiet;
        self.prev_cpu = Some(cpu);
        let processes = stats.len();
        self.stats = stats;
        self.tick_no += 1;
        let session_processes = sessions.iter().map(|s| s.procs).sum();
        let summary = Summary::of(&sessions);
        Ok(Snapshot {
            schema_version: SCHEMA_VERSION,
            taken_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            interval_ms: (secs * 1000.0).round() as u64,
            machine: Machine {
                cores,
                cpu_pct: machine_cpu,
                io_stall_source: if delayacct {
                    IoStallSource::Delayacct
                } else {
                    IoStallSource::DState
                },
                processes,
                session_processes,
                mem_total_bytes: machine.mem.mem_total,
                mem_available_bytes: machine.mem.mem_available,
                swap_total_bytes: machine.mem.swap_total,
                swap_free_bytes: machine.mem.swap_free,
                load_avg: machine.load,
                psi: machine.psi,
            },
            sessions,
            summary,
        })
    }

    /// Le io; status, smaps_rollup e `task/` conforme a cadencia (ver o topo do
    /// modulo). O `Option<f64>` e a CPU do processo (% de um nucleo, 0 se o
    /// contador nao andou) quando ele pode contar como quieto: tinha leitura
    /// anterior, nenhum contador de disco ou stall andou e nada esta em `D`.
    /// `None` se o processo sumiu.
    fn read_process(
        &mut self,
        root: &ProcRoot,
        st: &Stat,
        agent: u32,
        selected: bool,
        ctx: TickCtx,
    ) -> Option<(ProcessMetrics, Prev, Option<f64>)> {
        let TickCtx {
            delayacct,
            full,
            cpu_total,
            cores,
        } = ctx;
        let pid = st.pid;
        let tick = self.tick_no;
        let buf = &mut self.buf;
        // Mesmo processo do tick anterior (pid reusado tem outro starttime).
        let same = self.prev.get(&pid).filter(|p| p.starttime == st.starttime);
        let cached = same.map(|p| p.mem);
        let status = match same {
            Some(p) if !full && !selected => p.status,
            _ => {
                root.read_pid(pid, "status", buf).ok()?;
                parse_status(buf).ok()?
            }
        };
        // RSS do campo 24 do `stat`, ja lido neste tick: o mesmo contador do
        // `VmRSS`, sem o open do status. O `VmSwap` e o do ultimo status lido.
        let rss_bytes = st.rss_bytes();
        let rss = rss_bytes.unwrap_or(0);
        let swap = status.swap_bytes.unwrap_or(0);

        let read_at = same.map_or(tick, |p| p.read_at);
        let smaps_due = match cached {
            None => true,
            Some(_) if selected => true,
            Some(c) => {
                let age = tick - c.smaps_at;
                let limit = c.rss.max(1 << 20) >> SMAPS_MOVED_SHIFT;
                let moved = rss.abs_diff(c.rss) > limit || swap.abs_diff(c.swap) > limit;
                // A vez do pid, os ticks com `(tick + pid) % max_age == 0`, espalha
                // as releituras forcadas em vez de concentrar todas no mesmo tick.
                // Vale a vez que caiu desde a leitura anterior do processo: a
                // sessao quieta so e lida na varredura completa e perderia 4 de
                // cada 5 vezes, e todos os seus pids cairiam juntos nos 300 s.
                // Para quem e lido todo tick, a vez e este tick.
                let slot = tick.checked_sub((tick + u64::from(pid)) % self.max_age);
                let turn = slot.is_some_and(|s| s > read_at && s >= c.smaps_at + self.idle_every);
                let aged = age >= self.max_age || turn;
                age >= self.idle_every && (moved || aged)
            }
        };
        let (smaps, smaps_at, smaps_rss, smaps_swap) = match cached {
            Some(c) if !smaps_due => (c.smaps, c.smaps_at, c.rss, c.swap),
            _ => {
                let smaps = match root.read_pid(pid, "smaps_rollup", buf) {
                    Ok(()) => parse_smaps_rollup(buf).ok(),
                    Err(ReadError::Gone) => return None,
                    Err(_) => None,
                };
                (smaps, tick, rss, swap)
            }
        };
        let io = read_io(root, pid, buf);

        // Uma thread so: o estado do processo ja e o dela, sem ler task/.
        let (threads_d, d_at) = if delayacct {
            (0, tick)
        } else if st.num_threads <= 1 {
            (u32::from(st.state == 'D'), tick)
        } else {
            match cached {
                Some(c) if !selected && tick - c.d_at < self.idle_every => (c.threads_d, c.d_at),
                _ => (count_d_threads(root, pid, st.state, buf), tick),
            }
        };

        let cpu_ticks = st.utime + st.stime;
        let disk = |c: Option<IoCounters>| c.map(|c| (c.read_bytes, c.write_bytes));
        // Intervalo desde a leitura anterior deste processo, nao desde o tick
        // anterior: igual para quem e lido todo tick, a media do periodo para a
        // sessao que acorda.
        let d_total = same.map_or(0, |p| cpu_total.saturating_sub(p.cpu_total));
        let ticks_per_core = d_total as f64 / f64::from(cores);
        let secs = ticks_per_core / USER_HZ;
        let prev = same.filter(|_| d_total > 0);
        let pct = |d: u64| d as f64 / ticks_per_core * 100.0;
        // Sem anterior (primeiro tick, processo novo, pid reusado) e ausente, nao 0.
        let cpu_pct = prev.map(|p| pct(cpu_ticks.saturating_sub(p.cpu_ticks)));
        // CPU parada conta 0 mesmo sem intervalo (`cpu.total` parado); andando,
        // a media da janela, que precisa do intervalo.
        let quiet_cpu = same
            .filter(|p| {
                p.blkio_ticks == st.delayacct_blkio_ticks
                    && disk(p.io) == disk(io)
                    && st.state != 'D'
                    && threads_d == 0
            })
            .and_then(|p| match cpu_ticks.cmp(&p.cpu_ticks) {
                std::cmp::Ordering::Equal => Some(0.0),
                std::cmp::Ordering::Greater => cpu_pct,
                std::cmp::Ordering::Less => None,
            });
        let rate = |f: fn(&IoCounters) -> u64| -> Option<f64> {
            let (p, n) = (prev?.io?, io?);
            Some(f(&n).saturating_sub(f(&p)) as f64 / secs)
        };
        let (stall_pct, d_published) = if delayacct {
            let s = prev.map(|p| pct(st.delayacct_blkio_ticks.saturating_sub(p.blkio_ticks)));
            (s, None)
        } else {
            (None, Some(threads_d))
        };
        let m = ProcessMetrics {
            pid,
            ppid: st.ppid,
            comm: st.comm.clone(),
            state: st.state,
            uid: status.uid,
            // Campo 20 do `stat`, lido todo tick: o mesmo contador do `Threads`.
            threads: st.num_threads,
            cpu_pct,
            rss_bytes,
            pss_bytes: smaps.map(|s| s.pss),
            swap_bytes: smaps.map(|s| s.swap),
            swap_pss_bytes: smaps.map(|s| s.swap_pss),
            read_bps: rate(|c| c.read_bytes),
            write_bps: rate(|c| c.write_bytes),
            rchar_bps: rate(|c| c.rchar),
            wchar_bps: rate(|c| c.wchar),
            stall_pct,
            threads_d: d_published,
            voluntary_ctxt_switches: status.voluntary_ctxt_switches,
            nonvoluntary_ctxt_switches: status.nonvoluntary_ctxt_switches,
        };
        let now = Prev {
            starttime: st.starttime,
            cpu_ticks,
            blkio_ticks: st.delayacct_blkio_ticks,
            io,
            mem: MemCache {
                smaps,
                smaps_at,
                rss: smaps_rss,
                swap: smaps_swap,
                threads_d,
                d_at,
            },
            status,
            cpu_total,
            read_at: tick,
            agent,
            metrics: None,
        };
        Some((m, now, quiet_cpu))
    }
}

fn exe_basename(root: &ProcRoot, pid: u32) -> Option<String> {
    let target = std::fs::read_link(root.pid_path(pid, "exe")).ok()?;
    let name = Path::new(&target).file_name()?.to_string_lossy();
    // O kernel acrescenta " (deleted)" quando o binario foi substituido.
    Some(name.trim_end_matches(" (deleted)").to_string())
}

fn read_cmdline(root: &ProcRoot, pid: u32, buf: &mut String) -> Option<String> {
    root.read_pid(pid, "cmdline", buf).ok()?;
    let s: String = buf
        .trim_end_matches('\0')
        .chars()
        .map(|c| if c == '\0' { ' ' } else { c })
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Um tick entregue pela thread do sampler. Imutavel: o consumidor compartilha
/// sem copiar.
#[derive(Debug, Clone)]
pub struct Frame {
    pub snapshot: Arc<Snapshot>,
    /// Copia do historico apos este tick, se pedido em [`Sampler::spawn`].
    pub history: Option<Arc<History>>,
}

enum Control {
    Select(Option<u32>),
    Interval(Duration),
}

/// Ponta do consumidor. Soltar o handle para a thread (no maximo um tick depois).
pub struct SamplerHandle {
    rx: Option<Receiver<Result<Frame, ReadError>>>,
    ctl: Option<Sender<Control>>,
    thread: Option<JoinHandle<()>>,
}

impl SamplerHandle {
    /// Proximo frame, bloqueando. `None` se a thread acabou.
    pub fn recv(&self) -> Option<Result<Frame, ReadError>> {
        self.rx.as_ref()?.recv().ok()
    }

    /// Frame ja pronto, sem bloquear (para o laco de eventos da GUI).
    pub fn try_recv(&self) -> Option<Result<Frame, ReadError>> {
        self.rx.as_ref()?.try_recv().ok()
    }

    /// Ver [`Sampler::select`]; vale a partir do proximo tick.
    pub fn select(&self, agent_pid: Option<u32>) {
        if let Some(c) = &self.ctl {
            let _ = c.send(Control::Select(agent_pid));
        }
    }

    /// Troca o intervalo; o proximo tick ja sai no intervalo novo.
    pub fn set_interval(&self, interval: Duration) {
        if let Some(c) = &self.ctl {
            let _ = c.send(Control::Interval(interval));
        }
    }
}

impl Drop for SamplerHandle {
    fn drop(&mut self) {
        // Sem receptor o `send` falha; sem controle a espera acorda. Os dois
        // tiram a thread do laco, entao o join nao trava.
        self.rx.take();
        self.ctl.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    mut sampler: Sampler,
    root: ProcRoot,
    with_history: bool,
    tx: SyncSender<Result<Frame, ReadError>>,
    ctl: Receiver<Control>,
) {
    let mut next = Instant::now();
    loop {
        let frame = sampler.tick(&root).map(|snap| Frame {
            snapshot: Arc::new(snap),
            history: with_history.then(|| Arc::new(sampler.history().clone())),
        });
        if tx.send(frame).is_err() {
            return;
        }
        // Ritmo fixo: o custo do tick nao empurra os seguintes. Atrasado (tick
        // ou consumidor lento), nao recupera em rajada.
        next += sampler.config().interval;
        next = next.max(Instant::now());
        loop {
            match ctl.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(Control::Select(pid)) => sampler.select(pid),
                Ok(Control::Interval(d)) => {
                    sampler.set_interval(d);
                    next = Instant::now() + d;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}
