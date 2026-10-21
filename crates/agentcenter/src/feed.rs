//! Ponte entre a thread do sampler e o laco do GTK: "ultimo vence".
//!
//! `Sampler::spawn` entrega por `sync_channel(1)`: um consumidor atrasado
//! receberia o frame que ficou no canal, calculado antes do atraso. Aqui uma
//! thread de repasse esvazia o canal assim que cada frame chega e o grava num
//! slot unico, sobrescrevendo o anterior; a UI so e acordada quando o slot passa
//! de vazio a cheio. Atrasada quanto for, a UI le o frame mais novo (idade menor
//! que um tick) e nunca uma fila de frames velhos.

use agentcenter_core::procfs::ReadError;
use agentcenter_core::{Frame, HISTORY_LEN, History, SamplerHandle, Session, Snapshot};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Slot de um valor so; `put` sobrescreve.
#[derive(Debug)]
pub struct Latest<T> {
    slot: Mutex<Option<T>>,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }
}

impl<T> Latest<T> {
    /// Grava `v` no lugar do que houver. `true` se o slot estava vazio (quem
    /// consome precisa ser acordado).
    pub fn put(&self, v: T) -> bool {
        let mut s = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        s.replace(v).is_none()
    }

    pub fn take(&self) -> Option<T> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Repassa `next()` para `latest` ate a fonte acabar ou a UI soltar o receptor.
pub fn forward<T>(
    mut next: impl FnMut() -> Option<T>,
    latest: &Latest<T>,
    wake: &UnboundedSender<()>,
) {
    while let Some(v) = next() {
        if latest.put(v) && wake.unbounded_send(()).is_err() {
            return;
        }
    }
}

/// Thread de repasse dona de `source`. Devolve o slot e o aviso de "chegou frame".
pub fn spawn<T, S>(source: S, next: fn(&S) -> Option<T>) -> (Arc<Latest<T>>, UnboundedReceiver<()>)
where
    T: Send + 'static,
    S: Send + 'static,
{
    let latest = Arc::new(Latest::default());
    let (tx, rx) = unbounded();
    let slot = Arc::clone(&latest);
    std::thread::Builder::new()
        .name("agentcenter-feed".into())
        .spawn(move || forward(|| next(&source), &slot, &tx))
        .expect("spawn feed thread");
    (latest, rx)
}

/// Selecao pedida pela UI e ainda nao entregue ao sampler. A UI grava; a
/// thread de repasse, dona do [`SamplerHandle`], le antes de cada `recv`.
/// So o pedido mais novo importa: um clique por cima de outro substitui.
/// Tambem leva o intervalo novo do GSettings (C19), pelo mesmo caminho.
#[derive(Debug, Default)]
pub struct Selection {
    pending: Mutex<Option<Option<u32>>>,
    interval: Mutex<Option<Duration>>,
}

impl Selection {
    /// Pede a sessao `pid` (ou nenhuma) como selecionada no sampler.
    pub fn request(&self, pid: Option<u32>) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
    }

    /// O pedido pendente, uma vez.
    pub fn take(&self) -> Option<Option<u32>> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Pede outro intervalo de tick ao sampler.
    pub fn request_interval(&self, d: Duration) {
        *self.interval.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    }

    /// O intervalo pendente, uma vez.
    pub fn take_interval(&self) -> Option<Duration> {
        self.interval
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// Fonte do modo normal: o sampler e o pedido de selecao da UI. O
/// `SamplerHandle` nao e `Sync` (o receptor do canal), entao mora so nesta
/// thread; a UI fala com ele pelo [`Selection`]. O pedido sai antes do
/// proximo `recv` e o sampler o aplica antes do tick seguinte: a sessao
/// selecionada passa a ter smaps_rollup todo tick em no maximo dois ticks.
pub struct Live {
    handle: SamplerHandle,
    selection: Arc<Selection>,
}

impl Live {
    pub fn new(handle: SamplerHandle) -> (Self, Arc<Selection>) {
        let selection = Arc::new(Selection::default());
        (
            Self {
                handle,
                selection: Arc::clone(&selection),
            },
            selection,
        )
    }

    pub fn next(&self) -> Option<Result<Frame, ReadError>> {
        if let Some(pid) = self.selection.take() {
            self.handle.select(pid);
        }
        if let Some(d) = self.selection.take_interval() {
            self.handle.set_interval(d);
        }
        self.handle.recv()
    }
}

/// Modo fixture: com um arquivo aqui, a GUI nao sobe o sampler e publica este
/// `Snapshot` (JSON do `agentcenter-cli snapshot --json`) a cada tick.
pub const SNAPSHOT_ENV: &str = "AGENTCENTER_SNAPSHOT_JSON";
/// Intervalo do modo fixture em ms (padrao 1000, o do sampler).
pub const SNAPSHOT_INTERVAL_ENV: &str = "AGENTCENTER_SNAPSHOT_INTERVAL_MS";

const FIXTURE_INTERVAL: Duration = Duration::from_millis(1000);

/// Fonte de frames do modo fixture: o mesmo `Snapshot` imutavel a cada tick,
/// sem ler o procfs. O primeiro sai na hora, como o primeiro tick do sampler.
///
/// O snapshot nao tem passado, entao o historico do frame (faixas do detalhe,
/// C19) e sintetico e deterministico: [`synthetic_sample`] sobre os valores
/// do proprio snapshot, que e a amostra mais nova. Ja nasce com o ring cheio
/// e cada tick acrescenta uma amostra, entao o ring da volta na captura.
pub struct Fixture {
    snapshot: Arc<Snapshot>,
    interval: Duration,
    started: AtomicBool,
    history: Mutex<Synthetic>,
}

/// Historico sintetico do modo fixture e o relogio dele.
struct Synthetic {
    history: History,
    scratch: Vec<Session>,
    /// Ticks ja gravados; a amostra do snapshot e a de numero `HISTORY_LEN - 1`.
    tick: u64,
}

/// Fator da amostra `tick` de uma sessao: 1 no tick do snapshot, uma onda
/// lenta e uma rapida por pid em volta dele, nunca negativo. Deterministico.
pub fn synthetic_sample(pid: u32, tick: u64) -> f64 {
    let age = (HISTORY_LEN as f64 - 1.0) - tick as f64;
    let phase = f64::from(pid % 97) / 97.0 * std::f64::consts::TAU;
    let wave = |period: f64, amp: f64| {
        let w = std::f64::consts::TAU / period;
        amp * ((phase - age * w).sin() - phase.sin())
    };
    (1.0 + wave(240.0, 0.35) + wave(37.0, 0.15)).max(0.0)
}

impl Synthetic {
    fn new(snapshot: &Snapshot) -> Self {
        let mut s = Self {
            history: History::new(HISTORY_LEN),
            scratch: snapshot.sessions.clone(),
            tick: 0,
        };
        for _ in 0..HISTORY_LEN {
            s.push(snapshot);
        }
        s
    }

    fn push(&mut self, snapshot: &Snapshot) {
        let t = self.tick;
        for (out, base) in self.scratch.iter_mut().zip(&snapshot.sessions) {
            let f = synthetic_sample(base.pid, t);
            let fu = |v: Option<u64>| v.map(|b| (b as f64 * (0.85 + 0.15 * f)) as u64);
            out.cpu_pct = base.cpu_pct.map(|v| v * f);
            out.pss_bytes = fu(base.pss_bytes);
            out.swap_pss_bytes = fu(base.swap_pss_bytes);
            out.read_bps = base.read_bps.map(|v| v * f);
            out.write_bps = base.write_bps.map(|v| v * f * f);
            out.stall_pct = base.stall_pct.map(|v| v * f);
        }
        self.history.record(&self.scratch, |_| 0);
        self.tick += 1;
    }
}

impl Fixture {
    pub fn parse(json: &str, interval: Duration) -> Result<Self, String> {
        let snapshot: Snapshot = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let history = Mutex::new(Synthetic::new(&snapshot));
        Ok(Self {
            snapshot: Arc::new(snapshot),
            interval,
            started: AtomicBool::new(false),
            history,
        })
    }

    /// `None` sem a variavel; `Some(Err)` se o arquivo nao abre ou nao e um
    /// `Snapshot` (quem chama sai com erro: captura com fixture errada nao
    /// pode cair no sampler da maquina).
    pub fn from_env() -> Option<Result<Self, String>> {
        let path = std::env::var_os(SNAPSHOT_ENV).filter(|p| !p.is_empty())?;
        let interval = std::env::var(SNAPSHOT_INTERVAL_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map_or(FIXTURE_INTERVAL, Duration::from_millis);
        let shown = std::path::Path::new(&path).display().to_string();
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("{shown}: {e}"))
                .and_then(|json| Self::parse(&json, interval).map_err(|e| format!("{shown}: {e}"))),
        )
    }

    /// Proximo frame; dorme um intervalo antes de todos menos o primeiro.
    pub fn next(&self) -> Option<Result<Frame, ReadError>> {
        let mut h = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if self.started.swap(true, Ordering::Relaxed) {
            drop(h);
            std::thread::sleep(self.interval);
            h = self.history.lock().unwrap_or_else(|e| e.into_inner());
            h.push(&self.snapshot);
        }
        Some(Ok(Frame {
            snapshot: Arc::clone(&self.snapshot),
            history: Some(Arc::new(h.history.clone())),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
    use agentcenter_core::{Registry, Sampler, SamplerConfig, SamplerHandle};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn slow_consumer_gets_only_the_newest_value() {
        let latest = Latest::default();
        let (tx, mut rx) = unbounded();
        let mut values = 1..=5;
        forward(|| values.next(), &latest, &tx);
        assert_eq!(latest.take(), Some(5));
        // Um aviso so: o slot nunca voltou a vazio enquanto a UI dormia.
        assert_eq!(rx.try_recv().ok(), Some(()));
        assert!(rx.try_recv().is_err());
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn consumer_that_took_the_value_is_woken_again() {
        let latest = Latest::default();
        let (tx, mut rx) = unbounded();
        assert!(latest.put(1));
        assert_eq!(latest.take(), Some(1));
        let mut values = std::iter::once(2);
        forward(|| values.next(), &latest, &tx);
        assert_eq!(rx.try_recv().ok(), Some(()));
        assert_eq!(latest.take(), Some(2));
    }

    #[test]
    fn forwarding_stops_when_the_ui_is_gone() {
        let latest = Latest::default();
        let (tx, rx) = unbounded();
        drop(rx);
        let mut pulled = 0;
        forward(
            || {
                pulled += 1;
                Some(pulled)
            },
            &latest,
            &tx,
        );
        assert_eq!(pulled, 1);
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }

    /// Criterio herdado do C6: com a UI atrasada 3 s, o frame exibido tem idade
    /// menor que um tick. Aqui com o sampler de verdade sobre um procfs falso (fixture).
    #[test]
    fn ui_late_by_three_seconds_sees_a_frame_younger_than_one_tick() {
        let dir = TempDir::new("agentcenter-feed").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        fp.add(&FakeProcess::new(1, 0, "systemd")).unwrap();
        fp.add(&FakeProcess::new(10, 1, "claude")).unwrap();
        let tick = Duration::from_millis(500);
        let config = SamplerConfig {
            interval: tick,
            ..SamplerConfig::default()
        };
        let handle =
            Sampler::with_config(Registry::new(agentcenter_core::adapters::builtin()), config)
                .spawn(fp.proc_root(), false);
        let (latest, _wake) = spawn(handle, SamplerHandle::recv);
        // 3 s mais meio tick: no instante exato de um tick o mais novo ainda
        // pode ser o anterior (idade = 1 tick); no meio do intervalo, o frame
        // certo tem ~meio tick e um frame retido no canal teria 3 s.
        std::thread::sleep(Duration::from_secs(3) + tick / 2);
        let frame = latest.take().expect("a frame").expect("tick ok");
        let age = now_ms().saturating_sub(frame.snapshot.taken_at_ms);
        assert!(
            age < tick.as_millis() as u64,
            "frame age {age} ms, tick {} ms",
            tick.as_millis()
        );
    }

    /// C11: a selecao da UI chega ao sampler pela thread de repasse. Depois
    /// do pedido, so a sessao selecionada (e o filho dela) le smaps_rollup
    /// todo tick; a outra fica na cadencia de sessao quieta.
    #[test]
    fn ui_selection_reaches_the_sampler_through_the_feed() {
        let dir = TempDir::new("agentcenter-feed-select").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        fp.add(&FakeProcess::new(1, 0, "systemd")).unwrap();
        fp.add(&FakeProcess::new(20, 1, "claude")).unwrap();
        fp.add(&FakeProcess::new(21, 20, "node")).unwrap();
        fp.add(&FakeProcess::new(30, 1, "claude")).unwrap();
        let root = fp.counting_root();
        let config = SamplerConfig {
            interval: Duration::from_millis(20),
            ..SamplerConfig::default()
        };
        let handle =
            Sampler::with_config(Registry::new(agentcenter_core::adapters::builtin()), config)
                .spawn(root.clone(), false);
        let (live, selection) = Live::new(handle);
        assert!(live.next().is_some(), "first tick");
        selection.request(Some(20));
        assert_eq!(
            selection.take(),
            Some(Some(20)),
            "pending until the feed takes it"
        );
        selection.request(Some(20));
        // Pedido entregue no proximo next(); vale do tick seguinte em diante.
        for _ in 0..3 {
            live.next().unwrap().unwrap();
        }
        let (a, b, c) = (
            root.read_count(20, "smaps_rollup"),
            root.read_count(21, "smaps_rollup"),
            root.read_count(30, "smaps_rollup"),
        );
        for _ in 0..10 {
            live.next().unwrap().unwrap();
        }
        assert_eq!(root.read_count(20, "smaps_rollup") - a, 10, "selected");
        assert_eq!(
            root.read_count(21, "smaps_rollup") - b,
            10,
            "child of the selected"
        );
        assert!(
            root.read_count(30, "smaps_rollup") - c <= 2,
            "not selected: quiet cadence"
        );
        assert_eq!(selection.take(), None, "the request was delivered once");
    }

    fn fixture_files() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snapshots");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("fixtures dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn every_snapshot_fixture_parses_and_is_synthetic() {
        let files = fixture_files();
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_stem()?.to_str().map(str::to_owned))
            .collect();
        assert_eq!(
            names,
            ["cockpit", "nomes-longos", "tipico-40", "uma", "vazio"]
        );
        for path in &files {
            let json = std::fs::read_to_string(path).unwrap();
            let f = Fixture::parse(&json, FIXTURE_INTERVAL)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // Sinteticas: todo cwd sob o HOME inventado do gerador.
            for s in &f.snapshot.sessions {
                let cwd = s.cwd.as_deref().unwrap_or("/home/dev");
                assert!(cwd.starts_with("/home/dev"), "{}: {cwd}", path.display());
            }
        }
    }

    /// Chaves de um objeto JSON, recursivo nos objetos aninhados de forma fixa.
    /// `by_agent` e mapa (chave = id do adapter): conta so o nome do campo.
    fn shape(v: &serde_json::Value, path: &str, out: &mut std::collections::BTreeSet<String>) {
        if let serde_json::Value::Object(m) = v {
            for (k, child) in m {
                let p = format!("{path}.{k}");
                if k != "by_agent" {
                    shape(child, &p, out);
                }
                out.insert(p);
            }
        }
    }

    fn keys(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
        let mut out = Default::default();
        shape(v, "", &mut out);
        out
    }

    /// `#[serde(default)]` no `Summary` engole campo inventado e campo faltando:
    /// a fixture leria com zero sem erro. Aqui o formato e os numeros sao os do core.
    #[test]
    fn snapshot_fixtures_follow_the_core_summary_and_schema() {
        use agentcenter_core::Summary;
        use agentcenter_core::cockpit::{Cockpit, Thresholds, Tokens};
        use agentcenter_core::overlay::{HerdrOverlay, SessionOverlay};
        use agentcenter_core::snapshot::SCHEMA_VERSION;
        let summary_keys = keys(&serde_json::to_value(Summary::default()).unwrap());
        // `herdr` e `cockpit` sao `None` no default: o overlay confere so o
        // primeiro nivel e cada um, quando vem, confere contra o do core (o
        // cockpit nao tem default: um calculado de tokens zerados).
        let overlay_keys = keys(&serde_json::to_value(SessionOverlay::default()).unwrap());
        let herdr_keys = keys(&serde_json::to_value(HerdrOverlay::default()).unwrap());
        let cockpit = Cockpit::compute(&Thresholds::default(), &Tokens::default(), None, None);
        let cockpit_keys = keys(&serde_json::to_value(cockpit).unwrap());
        for path in fixture_files() {
            let name = path.display();
            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(
                raw["schema_version"],
                serde_json::json!(SCHEMA_VERSION),
                "{name}: schema_version"
            );
            assert_eq!(
                keys(&raw["summary"]),
                summary_keys,
                "{name}: chaves de summary"
            );
            for s in raw["sessions"].as_array().unwrap() {
                let o = &s["overlay"];
                if o.is_null() {
                    continue;
                }
                let top: std::collections::BTreeSet<_> = keys(o)
                    .into_iter()
                    .filter(|k| !k.starts_with(".herdr.") && !k.starts_with(".cockpit."))
                    .collect();
                assert_eq!(top, overlay_keys, "{name}: chaves de overlay");
                if !o["herdr"].is_null() {
                    assert_eq!(keys(&o["herdr"]), herdr_keys, "{name}: chaves de herdr");
                }
                if !o["cockpit"].is_null() {
                    assert_eq!(
                        keys(&o["cockpit"]),
                        cockpit_keys,
                        "{name}: chaves de cockpit"
                    );
                }
            }
            // Coerente com as sessoes: o mesmo que o core calcula delas.
            let snap: Snapshot = serde_json::from_value(raw).unwrap();
            let mut want = Summary::of(&snap.sessions);
            let mut got = snap.summary.clone();
            match (want.sessions_cpu_pct.take(), got.sessions_cpu_pct.take()) {
                (Some(w), Some(g)) => assert!((w - g).abs() < 1e-6, "{name}: cpu {g} != {w}"),
                (w, g) => assert_eq!(g, w, "{name}: sessions_cpu_pct"),
            }
            assert_eq!(got, want, "{name}: summary");
        }
    }

    #[test]
    fn typical_fixture_has_forty_sessions() {
        let path = fixture_files()
            .into_iter()
            .find(|p| p.ends_with("tipico-40.json"))
            .unwrap();
        let f = Fixture::parse(&std::fs::read_to_string(path).unwrap(), FIXTURE_INTERVAL).unwrap();
        assert_eq!(f.snapshot.sessions.len(), 40);
    }

    #[test]
    fn fixture_publishes_the_same_snapshot_first_at_once() {
        let json = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/snapshots/uma.json"),
        )
        .unwrap();
        let f = Fixture::parse(&json, Duration::from_millis(50)).unwrap();
        let t = std::time::Instant::now();
        let a = f.next().unwrap().unwrap();
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "first frame waits no tick"
        );
        let b = f.next().unwrap().unwrap();
        assert!(
            t.elapsed() >= Duration::from_millis(50),
            "later frames wait one tick"
        );
        assert!(Arc::ptr_eq(&a.snapshot, &b.snapshot), "no copy per tick");
        // C19: historico sintetico, ring ja cheio; a amostra mais nova do
        // primeiro frame e o proprio snapshot, e o segundo frame da a volta.
        let s = &a.snapshot.sessions[0];
        let (ha, hb) = (a.history.unwrap(), b.history.unwrap());
        let (ra, rb) = (ha.get(s.pid).unwrap(), hb.get(s.pid).unwrap());
        assert_eq!(ra.len(), HISTORY_LEN);
        assert_eq!(rb.len(), HISTORY_LEN);
        assert_eq!(
            ra.cpu_pct.latest().map(f64::from),
            s.cpu_pct.map(|v| f64::from(v as f32))
        );
        let (va, vb) = (ra.cpu_pct.to_vec(), rb.cpu_pct.to_vec());
        assert_eq!(
            va[1..],
            vb[..HISTORY_LEN - 1],
            "one sample in, the oldest out"
        );
    }

    #[test]
    fn synthetic_history_is_the_snapshot_now_and_never_negative() {
        for pid in [1, 42, 21520] {
            assert!((synthetic_sample(pid, HISTORY_LEN as u64 - 1) - 1.0).abs() < 1e-12);
            assert!((0..2000).all(|t| synthetic_sample(pid, t) >= 0.0));
        }
        assert_eq!(
            synthetic_sample(7, 10),
            synthetic_sample(7, 10),
            "deterministic"
        );
    }

    #[test]
    fn broken_fixture_is_an_error_not_a_fallback() {
        assert!(Fixture::parse("{}", FIXTURE_INTERVAL).is_err());
        assert!(Fixture::parse("not json", FIXTURE_INTERVAL).is_err());
    }
}
