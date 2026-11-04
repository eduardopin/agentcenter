//! Cost-weighted spend of the last [`SPEND_WINDOW_MIN`] minutes of a
//! transcript (RFC-AC-002): what the token map colours by and what the list
//! sorts by. The weights are the cockpit's ([`super::weighted`]), never raw
//! tokens.
//!
//! Each counted request lands in a one-minute bucket keyed by its transcript
//! line's timestamp, so the window is right from the first read of an old
//! transcript, not only after the app has watched it for half an hour.

use std::collections::VecDeque;

/// Width of the spend window, in minutes.
pub const SPEND_WINDOW_MIN: u64 = 30;
const MINUTE_MS: u64 = 60_000;

/// Weighted tokens per minute bucket, at most [`SPEND_WINDOW_MIN`] + 1 of
/// them, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpendWindow {
    buckets: VecDeque<(u64, f64)>,
}

impl SpendWindow {
    /// Adds `weighted` tokens spent at `at_ms` (ms since the epoch).
    /// Lines older than the window behind the newest bucket count nothing.
    pub fn add(&mut self, at_ms: u64, weighted: f64) {
        let min = at_ms / MINUTE_MS;
        let newest = self.buckets.back().map_or(min, |&(m, _)| m.max(min));
        if min + SPEND_WINDOW_MIN <= newest {
            return;
        }
        // Lines come in order; a late one walks back a few buckets at most.
        let at = self.buckets.partition_point(|&(m, _)| m < min);
        match self.buckets.get_mut(at) {
            Some((m, w)) if *m == min => *w += weighted,
            _ => self.buckets.insert(at, (min, weighted)),
        }
        while self
            .buckets
            .front()
            .is_some_and(|&(m, _)| m + SPEND_WINDOW_MIN <= newest)
        {
            self.buckets.pop_front();
        }
    }

    /// Weighted tokens of the last [`SPEND_WINDOW_MIN`] minutes before
    /// `now_ms`, the current minute included, truncated. Buckets after
    /// `now_ms`'s minute (a clock ahead of ours) wait for their minute.
    pub fn total(&self, now_ms: u64) -> u64 {
        let now = now_ms / MINUTE_MS;
        let from = (now + 1).saturating_sub(SPEND_WINDOW_MIN);
        self.buckets
            .iter()
            .filter(|&&(m, _)| (from..=now).contains(&m))
            .map(|&(_, w)| w)
            .sum::<f64>() as u64
    }

    /// Buckets held; bounded by the window, whatever the transcript's length.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_792_205_600_000;
    const MIN: u64 = MINUTE_MS;

    #[test]
    fn spend_inside_the_window_adds_up_and_older_spend_drops_out() {
        let mut w = SpendWindow::default();
        w.add(T0, 1_000.0);
        w.add(T0 + 10 * MIN, 500.0);
        w.add(T0 + 10 * MIN + 30_000, 250.5);
        assert_eq!(w.total(T0 + 10 * MIN), 1_750);
        assert_eq!(w.total(T0 + 29 * MIN), 1_750);
        assert_eq!(w.total(T0 + 30 * MIN), 750, "T0's minute is 30 minutes old");
        assert_eq!(w.total(T0 + 40 * MIN), 0);
    }

    #[test]
    fn a_long_transcript_keeps_only_the_window() {
        let mut w = SpendWindow::default();
        for m in 0..180 {
            w.add(T0 + m * MIN, 10.0);
        }
        assert!(
            w.len() <= SPEND_WINDOW_MIN as usize + 1,
            "{} buckets",
            w.len()
        );
        assert_eq!(w.total(T0 + 179 * MIN), 300);
    }

    #[test]
    fn late_lines_inside_the_window_count_and_older_ones_do_not() {
        let mut w = SpendWindow::default();
        w.add(T0 + 20 * MIN, 100.0);
        w.add(T0 + 5 * MIN, 40.0);
        w.add(T0 + 20 * MIN - 50 * MIN, 7.0);
        assert_eq!(w.total(T0 + 20 * MIN), 140);
        assert!(!w.is_empty());
    }

    #[test]
    fn an_empty_window_spends_nothing() {
        let w = SpendWindow::default();
        assert_eq!(w.total(T0), 0);
        assert!(w.is_empty());
    }
}
