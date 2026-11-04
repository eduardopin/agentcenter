//! Machine history for the direction B strip: one [`Ring`] per machine series,
//! [`HISTORY_LEN`] samples (10 min at 1 Hz), fed from each `Snapshot`'s
//! `machine`. No GTK, testable without a display.
//!
//! A missing reading (first tick without a CPU delta, no `/proc/meminfo` line,
//! a kernel without PSI, a machine without swap) is stored as `NaN`: the chart
//! breaks the line there and a gap never becomes 0.
//!
//! [`Readout`] holds the absolute values the strip prints for the newest
//! snapshot (GiB, cores, the other PSI readings), next to the rings.

use agentcenter_core::{HISTORY_LEN, Machine, Ring, Snapshot};

/// Machine CPU in % of the whole machine (0..=100).
pub fn cpu_pct(m: &Machine) -> Option<f64> {
    m.cpu_pct.filter(|v| v.is_finite())
}

/// `used` of `total` (saturating: a torn read never goes negative) and its
/// share in %. `None` without both readings or with a total of 0.
fn used_share(total: Option<u64>, free: Option<u64>) -> Option<(u64, f64)> {
    let (total, free) = (total?, free?);
    (total > 0).then(|| {
        let used = total.saturating_sub(free);
        (used, used as f64 / total as f64 * 100.0)
    })
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by tests; the strip takes the rings")
)]
/// RAM in use as % of the total: `(total - available) / total`.
pub fn ram_used_pct(m: &Machine) -> Option<f64> {
    used_share(m.mem_total_bytes, m.mem_available_bytes).map(|(_, p)| p)
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by tests; the strip takes the rings")
)]
/// Swap in use as % of the swap total. `None` without swap (0 of 0 is not a
/// reading).
pub fn swap_used_pct(m: &Machine) -> Option<f64> {
    used_share(m.swap_total_bytes, m.swap_free_bytes).map(|(_, p)| p)
}

/// PSI memory `some avg10`: % of the last 10 s in which some task stalled on
/// memory.
pub fn memory_stall_pct(m: &Machine) -> Option<f64> {
    m.psi.and_then(|p| p.memory).and_then(|r| r.some_avg10)
}

/// Absolute values of one snapshot, as the strip prints them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Readout {
    pub cores: u32,
    pub cpu_pct: Option<f64>,
    /// Sessions' CPU in cores (`summary.sessions_cpu_pct / 100`).
    pub sessions_cores: Option<f64>,
    pub mem_total: Option<u64>,
    pub mem_used: Option<u64>,
    pub mem_available: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_used: Option<u64>,
    pub ram_pct: Option<f64>,
    pub swap_pct: Option<f64>,
    /// PSI memory `some avg10`, `full avg10`; io and cpu `some avg10`.
    pub stall_some: Option<f64>,
    pub stall_full: Option<f64>,
    pub io_some: Option<f64>,
    pub cpu_some: Option<f64>,
}

impl Readout {
    pub fn of(snap: &Snapshot) -> Self {
        let m = &snap.machine;
        let ram = used_share(m.mem_total_bytes, m.mem_available_bytes);
        let swap = used_share(m.swap_total_bytes, m.swap_free_bytes);
        let psi = m.psi.unwrap_or_default();
        Self {
            cores: m.cores,
            cpu_pct: cpu_pct(m),
            sessions_cores: snap.summary.sessions_cpu_pct.map(|p| p / 100.0),
            mem_total: m.mem_total_bytes,
            mem_used: ram.map(|(u, _)| u),
            mem_available: m.mem_available_bytes,
            swap_total: m.swap_total_bytes,
            swap_used: swap.map(|(u, _)| u),
            ram_pct: ram.map(|(_, p)| p),
            swap_pct: swap.map(|(_, p)| p),
            stall_some: memory_stall_pct(m),
            stall_full: psi.memory.and_then(|r| r.full_avg10),
            io_some: psi.io.and_then(|r| r.some_avg10),
            cpu_some: psi.cpu.and_then(|r| r.some_avg10),
        }
    }
}

/// The four machine series, all the same length.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineHistory {
    pub cpu_pct: Ring,
    pub ram_pct: Ring,
    pub swap_pct: Ring,
    pub stall_pct: Ring,
    latest: Option<Readout>,
}

impl Default for MachineHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineHistory {
    /// [`HISTORY_LEN`] samples per series.
    pub fn new() -> Self {
        Self::with_capacity(HISTORY_LEN)
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cpu_pct: Ring::new(cap),
            ram_pct: Ring::new(cap),
            swap_pct: Ring::new(cap),
            stall_pct: Ring::new(cap),
            latest: None,
        }
    }

    /// One sample per series from `snap.machine`; full, the oldest drops.
    pub fn record(&mut self, snap: &Snapshot) {
        let r = Readout::of(snap);
        let f = |v: Option<f64>| v.map_or(f32::NAN, |x| x as f32);
        self.cpu_pct.push(f(r.cpu_pct));
        self.ram_pct.push(f(r.ram_pct));
        self.swap_pct.push(f(r.swap_pct));
        self.stall_pct.push(f(r.stall_some));
        self.latest = Some(r);
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by tests; the strip takes the rings")
    )]
    pub fn len(&self) -> usize {
        self.cpu_pct.len()
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by tests; the strip takes the rings")
    )]
    pub fn is_empty(&self) -> bool {
        self.cpu_pct.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cpu_pct.capacity()
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by tests; the strip takes the rings")
    )]
    /// Absolute values of the last recorded snapshot.
    pub fn latest(&self) -> Option<&Readout> {
        self.latest.as_ref()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use agentcenter_core::{Psi, PsiResource};

    pub(crate) fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
    }

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    /// jq '.machine' tipico-40.json: 38.0 GiB RAM with 8.7 available, 27.0
    /// GiB swap with 7.3 free, PSI memory some 12.5.
    #[test]
    fn percentages_of_the_typical_fixture() {
        let m = fixture("tipico-40").machine;
        assert_eq!(cpu_pct(&m), Some(41.8));
        let ram = ram_used_pct(&m).unwrap();
        assert!(
            (ram - 31_460_635_444.0 / 40_802_189_312.0 * 100.0).abs() < 1e-9,
            "{ram}"
        );
        assert_eq!(format!("{ram:.1}"), "77.1");
        let swap = swap_used_pct(&m).unwrap();
        assert!(
            (swap - 21_152_713_933.0 / 28_991_029_248.0 * 100.0).abs() < 1e-9,
            "{swap}"
        );
        assert_eq!(format!("{swap:.0}"), "73");
        assert_eq!(memory_stall_pct(&m), Some(12.5));
    }

    #[test]
    fn readout_of_the_typical_fixture() {
        let r = Readout::of(&fixture("tipico-40"));
        assert_eq!(r.cores, 16);
        assert_eq!(r.cpu_pct, Some(41.8));
        assert!((r.sessions_cores.unwrap() - 6.1834).abs() < 1e-9);
        assert_eq!(r.mem_total, Some(40_802_189_312));
        assert_eq!(r.mem_available, Some(9_341_553_868));
        assert_eq!(r.mem_used, Some(31_460_635_444));
        assert_eq!(format!("{:.1}", r.mem_used.unwrap() as f64 / GIB), "29.3");
        assert_eq!(r.swap_total, Some(28_991_029_248));
        assert_eq!(r.swap_used, Some(21_152_713_933));
        assert_eq!(format!("{:.1}", r.swap_used.unwrap() as f64 / GIB), "19.7");
        assert_eq!(r.stall_some, Some(12.5));
        assert_eq!(format!("{:.1}", r.stall_full.unwrap()), "4.2");
        assert_eq!(r.io_some, Some(3.1));
        assert_eq!(r.cpu_some, Some(1.1));
        assert_eq!(r.ram_pct, ram_used_pct(&fixture("tipico-40").machine));
    }

    #[test]
    fn missing_readings_are_none_never_zero() {
        let mut m = fixture("tipico-40").machine;
        m.cpu_pct = None;
        m.mem_available_bytes = None;
        m.swap_total_bytes = Some(0);
        m.swap_free_bytes = Some(0);
        m.psi = None;
        assert_eq!(cpu_pct(&m), None);
        assert_eq!(ram_used_pct(&m), None);
        assert_eq!(swap_used_pct(&m), None, "no swap is not 0 % used");
        assert_eq!(memory_stall_pct(&m), None);
        // PSI without the memory file, or a total of 0 RAM.
        m.psi = Some(Psi {
            cpu: Some(PsiResource::default()),
            memory: None,
            io: None,
        });
        assert_eq!(memory_stall_pct(&m), None);
        m.mem_total_bytes = Some(0);
        m.mem_available_bytes = Some(0);
        assert_eq!(ram_used_pct(&m), None);
    }

    #[test]
    fn available_above_total_clamps_to_zero_used() {
        // A torn read of /proc/meminfo must not draw a negative line.
        let mut m = fixture("tipico-40").machine;
        m.mem_available_bytes = m.mem_total_bytes.map(|t| t + 1);
        assert_eq!(ram_used_pct(&m), Some(0.0));
        m.swap_free_bytes = m.swap_total_bytes.map(|t| t + 1);
        assert_eq!(swap_used_pct(&m), Some(0.0));
    }

    #[test]
    fn record_pushes_one_sample_per_series() {
        let snap = fixture("tipico-40");
        let mut h = MachineHistory::new();
        assert!(h.is_empty());
        assert_eq!(h.capacity(), HISTORY_LEN);
        assert_eq!(h.latest(), None);
        h.record(&snap);
        assert_eq!(h.len(), 1);
        assert_eq!(h.cpu_pct.to_vec(), [41.8]);
        assert_eq!(
            h.ram_pct.latest(),
            Some(ram_used_pct(&snap.machine).unwrap() as f32)
        );
        assert_eq!(
            h.swap_pct.latest(),
            Some(swap_used_pct(&snap.machine).unwrap() as f32)
        );
        assert_eq!(h.stall_pct.to_vec(), [12.5]);
        assert_eq!(h.latest(), Some(&Readout::of(&snap)));
    }

    #[test]
    fn ring_wraps_after_capacity_and_keeps_the_newest() {
        let mut snap = fixture("tipico-40");
        let mut h = MachineHistory::with_capacity(4);
        for i in 0..7 {
            snap.machine.cpu_pct = Some(f64::from(i));
            h.record(&snap);
        }
        assert_eq!(h.len(), 4);
        assert_eq!(h.cpu_pct.to_vec(), [3.0, 4.0, 5.0, 6.0]);
        assert_eq!(h.ram_pct.len(), 4);
        assert_eq!(h.stall_pct.len(), 4);
        assert_eq!(h.latest().unwrap().cpu_pct, Some(6.0));
    }

    #[test]
    fn a_missing_reading_is_a_nan_sample_not_zero() {
        let mut snap = fixture("tipico-40");
        let mut h = MachineHistory::with_capacity(8);
        h.record(&snap);
        snap.machine.cpu_pct = None;
        snap.machine.psi = None;
        snap.machine.mem_available_bytes = None;
        snap.machine.swap_total_bytes = Some(0);
        h.record(&snap);
        assert!(h.cpu_pct.latest().unwrap().is_nan());
        assert!(h.ram_pct.latest().unwrap().is_nan());
        assert!(h.swap_pct.latest().unwrap().is_nan());
        assert!(h.stall_pct.latest().unwrap().is_nan());
        // The earlier sample is still there, untouched.
        assert_eq!(h.cpu_pct.to_vec()[0], 41.8);
        assert_eq!(h.len(), 2);
    }
}
