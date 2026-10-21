//! `/proc` falso para testes e bench: escreve so os arquivos que o leitor usa,
//! no mesmo formato do kernel. Nao e usado em producao.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Diretorio temporario apagado no `Drop` (evita depender do crate `tempfile`).
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> io::Result<Self> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Um processo falso. `Default` e um processo dormindo, sem CPU, com 1 thread.
#[derive(Debug, Clone)]
pub struct FakeProcess {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub state: char,
    pub utime: u64,
    pub stime: u64,
    pub threads: u32,
    pub starttime: u64,
    pub blkio_ticks: u64,
    pub uid: u32,
    pub rss_pages: u64,
    pub pss_kb: u64,
    pub swap_kb: u64,
    pub swap_pss_kb: u64,
    /// `None`: arquivo `io` ausente.
    pub io: Option<[u64; 4]>,
    pub cmdline: String,
    pub cwd: Option<String>,
    pub exe: Option<String>,
    /// Threads extras em estado D em `task/`.
    pub d_threads: u32,
}

impl Default for FakeProcess {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 1,
            comm: String::new(),
            state: 'S',
            utime: 0,
            stime: 0,
            threads: 1,
            starttime: 1000,
            blkio_ticks: 0,
            uid: 1000,
            rss_pages: 256,
            pss_kb: 1024,
            swap_kb: 0,
            swap_pss_kb: 0,
            io: Some([0; 4]),
            cmdline: String::new(),
            cwd: None,
            exe: None,
            d_threads: 0,
        }
    }
}

impl FakeProcess {
    pub fn new(pid: u32, ppid: u32, comm: &str) -> Self {
        Self {
            pid,
            ppid,
            comm: comm.to_string(),
            cmdline: comm.to_string(),
            ..Self::default()
        }
    }

    /// Linha de `stat` com os 52 campos do kernel atual.
    pub fn stat_line(&self) -> String {
        let mut f: Vec<String> = vec!["0".to_string(); 52];
        f[0] = self.pid.to_string();
        f[1] = format!("({})", self.comm);
        f[2] = self.state.to_string();
        f[3] = self.ppid.to_string();
        f[13] = self.utime.to_string();
        f[14] = self.stime.to_string();
        f[19] = self.threads.to_string();
        f[21] = self.starttime.to_string();
        f[23] = self.rss_pages.to_string();
        f[41] = self.blkio_ticks.to_string();
        f.join(" ") + "\n"
    }
}

/// Raiz `/proc` falsa.
#[derive(Debug)]
pub struct FakeProc {
    root: PathBuf,
}

impl FakeProc {
    /// Cria a raiz com `/proc/stat` (16 nucleos, ocioso), delayacct desligado,
    /// `meminfo` (64 GiB, 8 GiB de swap) e `loadavg`. Sem `pressure/`: kernel sem PSI.
    pub fn create(root: impl Into<PathBuf>) -> io::Result<Self> {
        let me = Self { root: root.into() };
        fs::create_dir_all(me.root.join("sys/kernel"))?;
        me.set_cpu(16, 0, 0)?;
        me.set_delayacct(false)?;
        me.set_meminfo(64 << 20, 32 << 20, 8 << 20, 6 << 20)?;
        fs::write(me.root.join("loadavg"), "1.50 0.75 0.25 2/870 12345\n")?;
        Ok(me)
    }

    /// Contador de leituras caras (ver [`crate::ProcRoot::counting`]).
    pub fn counting_root(&self) -> crate::ProcRoot {
        crate::ProcRoot::counting(&self.root)
    }

    /// Reescreve `/proc/meminfo` (valores em kB, como o kernel).
    pub fn set_meminfo(
        &self,
        total_kb: u64,
        avail_kb: u64,
        swap_total_kb: u64,
        swap_free_kb: u64,
    ) -> io::Result<()> {
        fs::write(
            self.root.join("meminfo"),
            format!(
                "MemTotal:       {total_kb} kB\nMemFree:        1024 kB\n\
                 MemAvailable:   {avail_kb} kB\nSwapCached:     0 kB\n\
                 SwapTotal:      {swap_total_kb} kB\nSwapFree:       {swap_free_kb} kB\n"
            ),
        )
    }

    /// Escreve `/proc/pressure/{cpu,memory,io}` com `some avg10=<some10>` e
    /// `full avg10=<some10/2>`; os avg60 sao metade dos avg10.
    pub fn set_pressure(&self, some10: f64) -> io::Result<()> {
        let dir = self.root.join("pressure");
        fs::create_dir_all(&dir)?;
        let full10 = some10 / 2.0;
        let body = format!(
            "some avg10={some10:.2} avg60={:.2} avg300=0.00 total=1\n\
             full avg10={full10:.2} avg60={:.2} avg300=0.00 total=1\n",
            some10 / 2.0,
            full10 / 2.0
        );
        for f in ["cpu", "memory", "io"] {
            fs::write(dir.join(f), &body)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn proc_root(&self) -> crate::ProcRoot {
        crate::ProcRoot::new(&self.root)
    }

    /// Reescreve `/proc/stat`: `busy` ticks em user, `idle` em idle, `cores` linhas cpuN.
    pub fn set_cpu(&self, cores: u32, busy: u64, idle: u64) -> io::Result<()> {
        let mut s = format!("cpu  {busy} 0 0 {idle} 0 0 0 0 0 0\n");
        for i in 0..cores {
            s.push_str(&format!("cpu{i} 0 0 0 0 0 0 0 0 0 0\n"));
        }
        s.push_str("intr 0\nctxt 0\nbtime 0\nprocesses 1\n");
        fs::write(self.root.join("stat"), s)
    }

    pub fn set_delayacct(&self, on: bool) -> io::Result<()> {
        fs::write(
            self.root.join("sys/kernel/task_delayacct"),
            if on { "1\n" } else { "0\n" },
        )
    }

    /// Escreve (ou reescreve) todos os arquivos do processo.
    pub fn add(&self, p: &FakeProcess) -> io::Result<()> {
        // `task/` tem uma entrada por thread: o total cobre a principal e as em D.
        let mut p = p.clone();
        p.threads = p.threads.max(1 + p.d_threads);
        let p = &p;
        let dir = self.root.join(p.pid.to_string());
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(dir.join("task"))?;
        fs::write(dir.join("stat"), p.stat_line())?;
        fs::write(
            dir.join("status"),
            format!(
                "Name:\t{}\nState:\t{}\nPPid:\t{}\nUid:\t{u}\t{u}\t{u}\t{u}\nThreads:\t{}\nVmRSS:\t{} kB\nVmSwap:\t{} kB\n\
                 voluntary_ctxt_switches:\t10\nnonvoluntary_ctxt_switches:\t2\n",
                p.comm,
                p.state,
                p.ppid,
                p.threads,
                p.rss_pages * 4,
                p.swap_kb,
                u = p.uid
            ),
        )?;
        fs::write(
            dir.join("statm"),
            format!("{} {} 0 0 0 0 0\n", p.rss_pages * 4, p.rss_pages),
        )?;
        fs::write(
            dir.join("smaps_rollup"),
            format!(
                "00400000-7ffd0000 ---p 00000000 00:00 0  [rollup]\nRss:  {} kB\nPss:  {} kB\n\
                 Swap:  {} kB\nSwapPss:  {} kB\nLocked:  0 kB\n",
                p.rss_pages * 4,
                p.pss_kb,
                p.swap_kb,
                p.swap_pss_kb
            ),
        )?;
        if let Some([r, w, rc, wc]) = p.io {
            fs::write(
                dir.join("io"),
                format!(
                    "rchar: {rc}\nwchar: {wc}\nsyscr: 0\nsyscw: 0\nread_bytes: {r}\n\
                     write_bytes: {w}\ncancelled_write_bytes: 0\n"
                ),
            )?;
        }
        let mut cmd = p.cmdline.replace(' ', "\0");
        cmd.push('\0');
        fs::write(dir.join("cmdline"), cmd)?;
        fs::write(dir.join("comm"), format!("{}\n", p.comm))?;
        if let Some(cwd) = &p.cwd {
            std::os::unix::fs::symlink(cwd, dir.join("cwd"))?;
        }
        if let Some(exe) = &p.exe {
            std::os::unix::fs::symlink(exe, dir.join("exe"))?;
        }
        // Thread principal (estado do processo), depois `d_threads` em D e o resto em S.
        let tdir = dir.join("task").join(p.pid.to_string());
        fs::create_dir_all(&tdir)?;
        fs::write(tdir.join("stat"), p.stat_line())?;
        for i in 0..p.threads - 1 {
            let mut t = p.clone();
            t.pid = p.pid * 1000 + i + 1;
            t.state = if i < p.d_threads { 'D' } else { 'S' };
            let tdir = dir.join("task").join(t.pid.to_string());
            fs::create_dir_all(&tdir)?;
            fs::write(tdir.join("stat"), t.stat_line())?;
        }
        Ok(())
    }

    /// Apaga um arquivo do processo (simula processo que sumiu no meio da leitura).
    pub fn remove_file(&self, pid: u32, file: &str) -> io::Result<()> {
        fs::remove_file(self.root.join(pid.to_string()).join(file))
    }
}

/// `/proc` sintetico com `n` processos: sessoes `claude` de ~20 processos
/// (shells, node de MCP) sob um `herdr`, e o resto solto sob o pid 1.
/// `tick` avanca os contadores para que dois ticks gerem deltas.
pub fn synthetic(
    root: impl Into<PathBuf>,
    n: u32,
    tick: u64,
    delayacct: bool,
) -> io::Result<FakeProc> {
    let fp = FakeProc::create(root)?;
    fp.set_delayacct(delayacct)?;
    fp.set_cpu(16, 1000 * tick, 15_000 * tick)?;
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    init.io = None;
    fp.add(&init)?;
    fp.add(&FakeProcess::new(2, 1, "herdr"))?;
    let mut pid = 3;
    let mut session_root = 0;
    while pid <= n {
        let (ppid, comm) = if pid > n * 3 / 4 {
            (1, "kworker/u32:1")
        } else if (pid - 3) % 20 == 0 {
            session_root = pid;
            (2, "claude")
        } else if (pid - 3) % 20 < 5 {
            (session_root, "bash")
        } else {
            (session_root, "node")
        };
        let mut p = FakeProcess::new(pid, ppid, comm);
        // Contagem de threads tipica: claude ~20, node de MCP ~8, shells 1.
        p.threads = match comm {
            "claude" => 20,
            "node" => 8,
            _ => 1,
        };
        p.utime = 3 * tick + u64::from(pid % 7);
        p.stime = tick;
        p.pss_kb = 1024 + u64::from(pid);
        p.io = Some([4096 * tick, 8192 * tick, 10_000 * tick, 20_000 * tick]);
        p.cwd = Some(format!("/home/dev/project-{session_root}"));
        fp.add(&p)?;
        pid += 1;
    }
    Ok(fp)
}
