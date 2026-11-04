//! Token spend per session, shared by the token map and the list
//! (RFC-AC-002): tokens since start, the 30-minute spend, its share of all
//! sessions' spend, tokens per minute and the violet heat bin.
//!
//! Every number is cost-weighted with the cockpit weights (the core's
//! `cockpit.burn_weighted` and `overlay.spend_30m`), never raw tokens. The
//! heat bin changes at most once per [`RECOLOUR_MS`] and only past a 10 %
//! margin around each bound, so a session hovering at 5k/min does not
//! flicker between two colours.

use agentcenter_core::Snapshot;
use std::collections::{HashMap, VecDeque};

/// Width of one history bucket, in ms: the sparkline has one point a minute.
pub const BUCKET_MS: u64 = 60_000;
/// Buckets kept per session: the 30 minutes the sparkline shows.
pub const HISTORY_POINTS: usize = 30;
/// Buckets without a snapshot after which the history restarts: the growth
/// in between cannot be placed, so it is not drawn as zeros or as a spike.
const MAX_GAP_BUCKETS: u64 = 2;

/// A session: pid and kernel start time (pids get reused).
pub type Key = (u32, Option<u64>);

/// Minutes the 30-minute spend is divided by for tokens per minute.
pub const WINDOW_MIN: f64 = 30.0;
/// Heat bins are recomputed at most this often, in ms.
pub const RECOLOUR_MS: u64 = 30_000;
/// Lower bounds of bins 2, 3 and 4, in weighted tokens per minute; bin 0 is
/// exactly zero and bin 1 anything above it below the first bound.
pub const BIN_BOUNDS: [f64; 3] = [1_000.0, 5_000.0, 20_000.0];
/// Margin a value must cross past a bound to change bin.
pub const HYSTERESIS: f64 = 0.10;
/// Share of the 30-minute spend above which one session raises the alert.
pub const ALERT_SHARE: f64 = 0.5;

/// What the tracker reads of one session on each snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Input {
    pub key: Key,
    /// `cockpit.burn_weighted`.
    pub since_start: Option<u64>,
    /// `overlay.spend_30m`.
    pub spend_30m: Option<u64>,
}

/// The tracker's input from one snapshot: every session, spend or not.
pub fn inputs(snap: &Snapshot) -> Vec<Input> {
    snap.sessions
        .iter()
        .map(|s| {
            let o = s.overlay.as_ref();
            Input {
                key: (s.pid, s.starttime),
                since_start: o.and_then(|o| o.cockpit.as_ref()).map(|c| c.burn_weighted),
                spend_30m: o.and_then(|o| o.spend_30m),
            }
        })
        .collect()
}

/// Weighted tokens per minute of one session, one bucket per wall-clock
/// minute, grown from the deltas of `since_start`.
#[derive(Debug, Clone, Default, PartialEq)]
struct History {
    /// Oldest first; the last one is the bucket of `bucket`.
    points: VecDeque<u64>,
    /// The `since_start` the next delta is taken from.
    last_burn: u64,
    /// Wall-clock minute of the last snapshot.
    bucket: u64,
    /// Minute of the baseline snapshot: the session was already running, so
    /// that minute is only partly seen and is never shown.
    start_bucket: u64,
}

impl History {
    fn start(burn: u64, now_ms: u64) -> Self {
        Self {
            points: VecDeque::new(),
            last_burn: burn,
            bucket: now_ms / BUCKET_MS,
            start_bucket: now_ms / BUCKET_MS,
        }
    }

    /// The minutes that are over and were seen whole: not the current one
    /// (still filling) and not the baseline's.
    fn complete(&self) -> Vec<u64> {
        let first_bucket = (self.bucket + 1).saturating_sub(self.points.len() as u64);
        let skip = usize::from(!self.points.is_empty() && first_bucket <= self.start_bucket);
        let end = self.points.len().saturating_sub(1);
        self.points.iter().copied().take(end).skip(skip).collect()
    }

    /// Takes the next reading; a gap restarts, a smaller burn is a reset.
    fn push(&mut self, burn: u64, now_ms: u64) {
        let bucket = now_ms / BUCKET_MS;
        if bucket.saturating_sub(self.bucket) > MAX_GAP_BUCKETS || bucket < self.bucket {
            *self = Self::start(burn, now_ms);
            return;
        }
        let gain = burn.saturating_sub(self.last_burn);
        if self.points.is_empty() || bucket > self.bucket {
            // Minutes skipped inside the allowed gap are true idle minutes.
            let new = if self.points.is_empty() {
                1
            } else {
                bucket - self.bucket
            };
            for _ in 0..new {
                self.points.push_back(0);
            }
            // One more than shown: the newest is still filling.
            while self.points.len() > HISTORY_POINTS + 1 {
                self.points.pop_front();
            }
        }
        if let Some(last) = self.points.back_mut() {
            *last += gain;
        }
        self.last_burn = burn;
        self.bucket = bucket;
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Entry {
    history: Option<History>,
    since_start: Option<u64>,
    spend_30m: Option<u64>,
    bin: Option<u8>,
}

#[derive(Debug, Default)]
pub struct SpendTracker {
    entries: HashMap<Key, Entry>,
    total_30m: u64,
    binned_at_ms: Option<u64>,
}

/// Heat bin of `per_min` given the bin it had, with [`HYSTERESIS`].
pub fn bin_of(per_min: f64, prev: Option<u8>) -> u8 {
    let raw = |v: f64| {
        if v <= 0.0 {
            0
        } else {
            1 + BIN_BOUNDS.iter().filter(|&&b| v >= b).count() as u8
        }
    };
    let bin = raw(per_min);
    match prev {
        // Zero and the first bin have no margin: no spend is no spend.
        Some(p) if bin != 0 && p != 0 && bin > p => raw(per_min / (1.0 + HYSTERESIS)).max(p),
        Some(p) if bin != 0 && p != 0 && bin < p => raw(per_min / (1.0 - HYSTERESIS)).min(p),
        _ => bin,
    }
}

impl SpendTracker {
    /// Takes the sessions of one snapshot; sessions not in `rows` are dropped.
    pub fn update(&mut self, now_ms: u64, rows: impl IntoIterator<Item = Input>) {
        let recolour = self
            .binned_at_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= RECOLOUR_MS);
        if recolour {
            self.binned_at_ms = Some(now_ms);
        }
        let mut old = std::mem::take(&mut self.entries);
        for r in rows {
            let was = old.remove(&r.key);
            let prev = was.as_ref().and_then(|e| e.bin);
            let bin = r.spend_30m.map(|s| match prev {
                Some(p) if !recolour => p,
                _ => bin_of(s as f64 / WINDOW_MIN, prev),
            });
            let history = match (r.since_start, was.and_then(|e| e.history)) {
                (None, _) => None,
                (Some(b), None) => Some(History::start(b, now_ms)),
                (Some(b), Some(mut h)) => {
                    h.push(b, now_ms);
                    Some(h)
                }
            };
            self.entries.insert(
                r.key,
                Entry {
                    history,
                    since_start: r.since_start,
                    spend_30m: r.spend_30m,
                    bin,
                },
            );
        }
        self.total_30m = self.entries.values().filter_map(|e| e.spend_30m).sum();
    }

    /// Weighted tokens per minute for the last [`HISTORY_POINTS`] complete
    /// minutes, oldest first; `None` when the burn is unknown or no minute
    /// has closed yet (the baseline's minute and the current one are left
    /// out, so a steady rate does not read as a dip at either end).
    pub fn series(&self, k: Key) -> Option<Vec<u64>> {
        let h = self.entries.get(&k)?.history.as_ref()?;
        Some(h.complete()).filter(|v| !v.is_empty())
    }

    /// Sessions the buffer holds.
    pub fn tracked(&self) -> usize {
        self.entries.len()
    }

    /// Points held over all sessions.
    pub fn history_points(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| e.history.as_ref())
            .map(|h| h.points.len())
            .sum()
    }

    pub fn since_start(&self, k: Key) -> Option<u64> {
        self.entries.get(&k).and_then(|e| e.since_start)
    }

    pub fn spend_30m(&self, k: Key) -> Option<u64> {
        self.entries.get(&k).and_then(|e| e.spend_30m)
    }

    /// Weighted tokens per minute over the window.
    pub fn per_min(&self, k: Key) -> Option<f64> {
        self.spend_30m(k).map(|s| s as f64 / WINDOW_MIN)
    }

    /// The session's part of all known 30-minute spend, 0..=1; `None` when
    /// its spend is unknown.
    pub fn share(&self, k: Key) -> Option<f64> {
        let s = self.spend_30m(k)?;
        Some(if self.total_30m == 0 {
            0.0
        } else {
            s as f64 / self.total_30m as f64
        })
    }

    /// Heat bin 0..=4; `None` when the spend is unknown.
    pub fn bin(&self, k: Key) -> Option<u8> {
        self.entries.get(&k).and_then(|e| e.bin)
    }

    /// Weighted tokens per minute of all sessions.
    pub fn total_per_min(&self) -> f64 {
        self.total_30m as f64 / WINDOW_MIN
    }

    /// The biggest 30-minute spender and its share; `None` when nobody spent.
    pub fn top(&self) -> Option<(Key, f64)> {
        // Ties go to the lower key, so the top does not hop between
        // HashMap orders from one snapshot to the next.
        let (k, _) = self
            .entries
            .iter()
            .filter_map(|(k, e)| e.spend_30m.filter(|&s| s > 0).map(|s| (*k, s)))
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
        Some((k, self.share(k)?))
    }

    /// The session that takes more than [`ALERT_SHARE`] of the spend, when
    /// at least two sessions have a known spend.
    pub fn alert(&self) -> Option<Key> {
        let known = self
            .entries
            .values()
            .filter(|e| e.spend_30m.is_some())
            .count();
        self.top()
            .filter(|&(_, share)| known >= 2 && share > ALERT_SHARE)
            .map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Key = (100, Some(1));
    const B: Key = (200, Some(2));
    const C: Key = (300, None);

    fn row(key: Key, since: u64, spend: Option<u64>) -> Input {
        Input {
            key,
            since_start: Some(since),
            spend_30m: spend,
        }
    }

    #[test]
    fn inputs_read_burn_and_spend_from_the_cockpit_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/snapshots/cockpit.json"
        );
        let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let rows = inputs(&snap);
        assert_eq!(rows.len(), snap.sessions.len());
        let known: Vec<_> = rows.iter().filter(|r| r.since_start.is_some()).collect();
        assert!(!known.is_empty());
        assert!(
            known
                .iter()
                .all(|r| r.spend_30m == Some(r.since_start.unwrap() / 20))
        );
        let mut t = SpendTracker::default();
        t.update(0, rows);
        assert!(t.top().is_some());
    }

    #[test]
    fn share_is_the_true_part_of_the_known_spend() {
        let mut t = SpendTracker::default();
        t.update(
            0,
            [
                row(A, 9_000, Some(3_000)),
                row(B, 50, Some(1_000)),
                row(C, 7, None),
            ],
        );
        assert_eq!(t.share(A), Some(0.75));
        assert_eq!(t.share(B), Some(0.25));
        assert_eq!(t.share(C), None, "no transcript");
        assert_eq!(t.since_start(A), Some(9_000));
        assert_eq!(t.spend_30m(B), Some(1_000));
        assert_eq!(t.per_min(A), Some(100.0));
        assert_eq!(t.total_per_min(), 4_000.0 / 30.0);
    }

    #[test]
    fn nobody_spending_gives_zero_shares_and_no_top() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 10, Some(0)), row(B, 10, Some(0))]);
        assert_eq!(t.share(A), Some(0.0));
        assert_eq!(t.top(), None);
        assert_eq!(t.alert(), None);
    }

    #[test]
    fn sessions_that_end_leave_the_tracker() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 1, Some(10)), row(B, 1, Some(30))]);
        t.update(1_000, [row(B, 1, Some(30))]);
        assert_eq!(t.since_start(A), None);
        assert_eq!(t.share(B), Some(1.0));
    }

    #[test]
    fn top_spender_and_the_alert_past_half_the_spend() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 1, Some(600)), row(B, 1, Some(400))]);
        assert_eq!(t.top(), Some((A, 0.6)));
        assert_eq!(t.alert(), Some(A));
        t.update(1_000, [row(A, 1, Some(500)), row(B, 1, Some(500))]);
        assert_eq!(t.alert(), None, "exactly half is not more than half");
        t.update(2_000, [row(A, 1, Some(900))]);
        assert_eq!(t.alert(), None, "one session alone is not an alert");
    }

    #[test]
    fn bins_follow_the_fixed_bounds() {
        assert_eq!(bin_of(0.0, None), 0);
        assert_eq!(bin_of(0.5, None), 1);
        assert_eq!(bin_of(999.0, None), 1);
        assert_eq!(bin_of(1_000.0, None), 2);
        assert_eq!(bin_of(4_999.0, None), 2);
        assert_eq!(bin_of(5_000.0, None), 3);
        assert_eq!(bin_of(20_000.0, None), 4);
        assert_eq!(bin_of(1e9, None), 4);
    }

    #[test]
    fn bins_move_only_past_a_ten_percent_margin() {
        assert_eq!(bin_of(5_200.0, Some(2)), 2, "up needs 5500");
        assert_eq!(bin_of(5_600.0, Some(2)), 3);
        assert_eq!(bin_of(4_700.0, Some(3)), 3, "down needs below 4500");
        assert_eq!(bin_of(4_400.0, Some(3)), 2);
        assert_eq!(bin_of(25_000.0, Some(1)), 4, "a jump crosses every bound");
        assert_eq!(bin_of(0.0, Some(3)), 0, "zero is zero");
        assert_eq!(bin_of(10.0, Some(0)), 1);
    }

    #[test]
    fn bins_recolour_only_on_the_thirty_second_tick() {
        let mut t = SpendTracker::default();
        // 30 * 3000/min = 90k: bin 2.
        t.update(0, [row(A, 1, Some(90_000))]);
        assert_eq!(t.bin(A), Some(2));
        // 30 * 6000/min: bin 3, but only 10 s later.
        t.update(10_000, [row(A, 1, Some(180_000)), row(B, 1, Some(0))]);
        assert_eq!(t.bin(A), Some(2), "held until the tick");
        assert_eq!(t.bin(B), Some(0), "a new session gets its bin at once");
        t.update(30_000, [row(A, 1, Some(180_000)), row(B, 1, Some(0))]);
        assert_eq!(t.bin(A), Some(3));
        assert_eq!(t.bin(C), None);
        t.update(40_000, [row(A, 1, None)]);
        assert_eq!(t.bin(A), None, "unknown spend has no colour");
    }

    // ---- 30-minute history behind the row sparkline (RFC-AC-003, acceptance 4)
    //
    // Contract under test: `series(k)` is the session's weighted tokens per
    // minute, one value per wall-clock minute (`now_ms / BUCKET_MS`), oldest
    // first, at most `HISTORY_POINTS` long, and only COMPLETE minutes: the
    // minute of the last `update` is still filling and the minute of the
    // session's first snapshot was only partly seen, so neither is shown.
    // A value is the sum of the `since_start` growth seen by snapshots that
    // fell in that minute. The first snapshot of a session is a baseline.

    const MIN: u64 = BUCKET_MS;

    #[test]
    fn history_is_thirty_minute_buckets() {
        assert_eq!(HISTORY_POINTS, 30);
        assert_eq!(BUCKET_MS, 60_000);
    }

    #[test]
    fn series_is_the_growth_of_the_burn_per_minute_not_the_30_minute_sum() {
        let mut t = SpendTracker::default();
        // Snapshots every 30 s; spend_30m stays a constant 90_000 on purpose:
        // the series must come from the burn deltas, not from it.
        for (at, burn) in [
            (0, 1_000),
            (30_000, 1_100),
            (60_000, 1_300),
            (90_000, 1_600),
            (120_000, 1_700),
        ] {
            t.update(at, [row(A, burn, Some(90_000))]);
        }
        // Minute 0 is the baseline's, minute 2 is still filling; minute 1
        // saw +200 and +300.
        assert_eq!(t.series(A), Some(vec![500]));
    }

    #[test]
    fn the_first_snapshot_is_a_baseline_and_draws_nothing() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 5_000_000, Some(10))]);
        assert_eq!(t.series(A), None, "one point has no growth to show");
        t.update(MIN, [row(A, 5_000_600, Some(10))]);
        assert_eq!(t.series(A), None, "no minute has closed whole yet");
        t.update(2 * MIN, [row(A, 5_001_200, Some(10))]);
        assert_eq!(t.series(A), Some(vec![600]), "the old burn is not a spike");
    }

    #[test]
    fn an_idle_minute_is_a_true_zero_between_two_known_readings() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 100, Some(1))]);
        t.update(MIN, [row(A, 400, Some(1))]);
        t.update(2 * MIN, [row(A, 400, Some(1))]);
        t.update(3 * MIN, [row(A, 700, Some(1))]);
        t.update(4 * MIN, [row(A, 700, Some(1))]);
        assert_eq!(t.series(A), Some(vec![300, 0, 300]));
    }

    #[test]
    fn a_gap_of_missed_snapshots_restarts_the_history_instead_of_faking_zeros() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 100, Some(1))]);
        t.update(MIN, [row(A, 200, Some(1))]);
        t.update(2 * MIN, [row(A, 300, Some(1))]);
        assert_eq!(t.series(A), Some(vec![100]));
        // Suspended for 10 minutes: where the growth went cannot be told,
        // so the snapshot after the gap is a new baseline, not a spike and
        // not nine zeros.
        t.update(12 * MIN, [row(A, 5_300, Some(1))]);
        assert_eq!(t.series(A), None);
        t.update(13 * MIN, [row(A, 5_400, Some(1))]);
        t.update(14 * MIN, [row(A, 5_500, Some(1))]);
        assert_eq!(t.series(A), Some(vec![100]));
    }

    #[test]
    fn a_gap_of_exactly_two_minutes_is_kept_and_a_longer_one_restarts() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 0, Some(1))]);
        t.update(MIN, [row(A, 100, Some(1))]);
        t.update(2 * MIN, [row(A, 200, Some(1))]);
        // Two minutes without a snapshot: still one history, with the two
        // skipped minutes as idle zeros.
        t.update(4 * MIN, [row(A, 500, Some(1))]);
        t.update(5 * MIN, [row(A, 500, Some(1))]);
        assert_eq!(t.series(A), Some(vec![100, 100, 0, 300]));
        // Three minutes without one: a restart, nothing carried over.
        t.update(8 * MIN, [row(A, 900, Some(1))]);
        assert_eq!(t.series(A), None);
        t.update(9 * MIN, [row(A, 1_000, Some(1))]);
        t.update(10 * MIN, [row(A, 1_100, Some(1))]);
        assert_eq!(t.series(A), Some(vec![100]));
    }

    #[test]
    fn a_snapshot_at_an_earlier_minute_restarts_the_history() {
        let mut t = SpendTracker::default();
        for m in 10..14u64 {
            t.update(m * MIN, [row(A, m * 100, Some(1))]);
        }
        assert_eq!(t.series(A), Some(vec![100, 100]));
        // The clock goes back one minute: not a negative minute, not a
        // wrapped gap, and the old points are not kept under the new clock.
        t.update(12 * MIN, [row(A, 1_400, Some(1))]);
        assert_eq!(t.series(A), None);
        t.update(13 * MIN, [row(A, 1_500, Some(1))]);
        t.update(14 * MIN, [row(A, 1_600, Some(1))]);
        assert_eq!(t.series(A), Some(vec![100]));
    }

    #[test]
    fn a_steady_rate_reads_the_same_before_and_after_a_minute_rolls_over() {
        let mut t = SpendTracker::default();
        let mut burn = 0;
        // 600 a minute, a snapshot every 10 s, from a baseline mid-minute.
        for at in (5_000..=400_000u64).step_by(10_000) {
            t.update(at, [row(A, burn, Some(1))]);
            burn += 100;
            if let Some(s) = t.series(A) {
                assert!(s.iter().all(|&v| v == 600), "at {at} ms: {s:?}");
            }
        }
        // Just before the rollover to minute 6 and just after it.
        for at in [354_999u64, 365_000] {
            let mut t2 = SpendTracker::default();
            let mut burn = 0;
            for tick in (5_000..=at).step_by(10_000) {
                t2.update(tick, [row(A, burn, Some(1))]);
                burn += 100;
            }
            let s = t2.series(A).unwrap();
            assert!(s.iter().all(|&v| v == 600), "at {at} ms: {s:?}");
            assert_eq!(s.last(), Some(&600), "now is 600 at {at} ms");
        }
    }

    #[test]
    fn the_buffer_keeps_thirty_minutes_and_no_more() {
        let mut t = SpendTracker::default();
        for m in 0..=45u64 {
            t.update(m * MIN, [row(A, 1_000 + m * 100, Some(1))]);
        }
        let s = t.series(A).unwrap();
        assert_eq!(s.len(), HISTORY_POINTS);
        assert!(s.iter().all(|&v| v == 100), "{s:?}");
        // Old minutes leave from the front: a burst in minute 46 is shown
        // once minute 47 opens.
        t.update(46 * MIN, [row(A, 1_000 + 45 * 100 + 9_000, Some(1))]);
        t.update(47 * MIN, [row(A, 1_000 + 45 * 100 + 9_000, Some(1))]);
        let s = t.series(A).unwrap();
        assert_eq!((s.len(), *s.last().unwrap()), (HISTORY_POINTS, 9_000));
    }

    #[test]
    fn many_snapshots_in_one_minute_stay_one_point() {
        let mut t = SpendTracker::default();
        for k in 0..180u64 {
            t.update(k * 1_000, [row(A, 1_000 + k * 10, Some(1))]);
        }
        let s = t.series(A).unwrap();
        assert_eq!(s, vec![600], "one point a minute, not one a snapshot");
    }

    #[test]
    fn a_burn_that_goes_down_adds_nothing_instead_of_wrapping() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 9_000, Some(1))]);
        t.update(MIN, [row(A, 100, Some(1))]);
        t.update(2 * MIN, [row(A, 100, Some(1))]);
        assert_eq!(t.series(A), Some(vec![0]), "a reset, not u64::MAX");
        t.update(3 * MIN, [row(A, 400, Some(1))]);
        t.update(4 * MIN, [row(A, 400, Some(1))]);
        assert_eq!(
            t.series(A),
            Some(vec![0, 0, 300]),
            "growth counts from the reset"
        );
    }

    #[test]
    fn an_unknown_burn_has_no_series_and_no_stale_baseline() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 1_000, Some(1))]);
        t.update(MIN, [row(A, 1_500, Some(1))]);
        t.update(2 * MIN, [row(A, 1_500, Some(1))]);
        assert_eq!(t.series(A), Some(vec![500]));
        let unknown = Input {
            key: A,
            since_start: None,
            spend_30m: None,
        };
        t.update(3 * MIN, [unknown]);
        assert_eq!(t.series(A), None, "no transcript: no line, not a flat zero");
        // The burn comes back much higher: the unseen growth is not a spike.
        t.update(4 * MIN, [row(A, 90_000, Some(1))]);
        assert_eq!(t.series(A), None, "a new baseline, nothing to draw yet");
        t.update(5 * MIN, [row(A, 90_250, Some(1))]);
        t.update(6 * MIN, [row(A, 90_250, Some(1))]);
        assert_eq!(t.series(A), Some(vec![250]));
    }

    #[test]
    fn a_session_never_seen_has_no_series() {
        let mut t = SpendTracker::default();
        t.update(0, [row(A, 1, Some(1))]);
        assert_eq!(t.series(B), None);
        assert_eq!(t.series(C), None);
    }

    #[test]
    fn ended_sessions_lose_their_history_and_a_reused_key_starts_empty() {
        let mut t = SpendTracker::default();
        for m in 0..5u64 {
            t.update(m * MIN, [row(A, m * 100, Some(1)), row(B, m * 10, Some(1))]);
        }
        assert!(t.series(A).is_some() && t.series(B).is_some());
        t.update(5 * MIN, [row(B, 50, Some(1))]);
        assert_eq!(t.series(A), None, "ended: dropped");
        assert_eq!(t.tracked(), 1, "the buffer holds only live sessions");
        // The same key coming back (it should not, but a snapshot can blink)
        // starts from nothing rather than resurrecting old minutes.
        t.update(6 * MIN, [row(A, 9_000, Some(1)), row(B, 60, Some(1))]);
        assert_eq!(t.series(A), None);
    }

    #[test]
    fn a_reused_pid_with_another_start_time_is_another_history() {
        let old: Key = (100, Some(1));
        let new: Key = (100, Some(99));
        let mut t = SpendTracker::default();
        t.update(0, [row(old, 0, Some(1))]);
        t.update(MIN, [row(old, 500, Some(1))]);
        t.update(2 * MIN, [row(new, 0, Some(1))]);
        assert_eq!(t.series(old), None);
        assert_eq!(t.series(new), None, "nothing of the old process leaks in");
        assert_eq!(t.tracked(), 1);
    }

    #[test]
    fn the_buffer_is_bounded_for_a_long_run_of_many_sessions() {
        let mut t = SpendTracker::default();
        for m in 0..120u64 {
            // 50 sessions, a quarter of them replaced every minute.
            let rows: Vec<_> = (0..50u32)
                .map(|i| row((i + (m as u32) / 4 * 12, Some(1)), m * 7, Some(1)))
                .collect();
            t.update(m * MIN, rows);
        }
        assert_eq!(t.tracked(), 50);
        assert!(
            t.history_points() <= 50 * (HISTORY_POINTS + 1),
            "{}",
            t.history_points()
        );
    }
}
