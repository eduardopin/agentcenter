//! When the list may change its order (REVIEW_RR1 "Spend and order"): at most
//! once per [`MIN_INTERVAL_MS`], never while the pointer is over the list,
//! focus is in it, a dialog is open or an operation runs. A reorder that
//! arrives during a hold waits and applies when the hold ends. Pure logic with
//! the clock passed in, so it needs no display and no timer to test.

/// Shortest time between two reorders.
pub const MIN_INTERVAL_MS: u64 = 30_000;

/// What currently keeps the list still.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holds {
    pub pointer: bool,
    pub focus: bool,
    pub dialog: bool,
    pub operation: bool,
}

impl Holds {
    pub fn any(self) -> bool {
        self.pointer || self.focus || self.dialog || self.operation
    }
}

#[derive(Debug, Default)]
pub struct Gate {
    applied_at: Option<u64>,
    waiting: bool,
}

impl Gate {
    /// A reorder is wanted: the order of the data differs from the screen.
    pub fn want(&mut self) {
        self.waiting = true;
    }

    /// `true` when the waiting reorder may apply now; the caller then applies
    /// it. The first reorder of a list is never delayed.
    pub fn poll(&mut self, now_ms: u64, holds: Holds) -> bool {
        if !self.waiting || holds.any() {
            return false;
        }
        if self
            .applied_at
            .is_some_and(|at| now_ms.saturating_sub(at) < MIN_INTERVAL_MS)
        {
            return false;
        }
        self.force(now_ms);
        true
    }

    /// An order applied outside the gate (the user picked another order, F5):
    /// restarts the interval and drops what was waiting.
    pub fn force(&mut self, now_ms: u64) {
        self.applied_at = Some(now_ms);
        self.waiting = false;
    }

    #[cfg(test)]
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// Whole seconds since the order was applied; `None` before the first.
    pub fn age_s(&self, now_ms: u64) -> Option<u64> {
        self.applied_at.map(|at| now_ms.saturating_sub(at) / 1000)
    }
}

/// `Sorted by 30-min spend - updated 12 s ago`.
pub fn sorted_by_text(order: &str, age_s: Option<u64>) -> String {
    match age_s {
        Some(s) => format!("Sorted by {order} - updated {s} s ago"),
        None => format!("Sorted by {order}"),
    }
}

/// Scroll value that keeps a row at the same screen position after it moved
/// from `old_pos` to `new_pos` in a list of rows `row_h` tall.
pub fn keep_position(value: f64, old_pos: u32, new_pos: u32, row_h: f64) -> f64 {
    (value + (f64::from(new_pos) - f64::from(old_pos)) * row_h).max(0.0)
}

/// The rank cell of a row: `#1`, `#2`; empty before the first sort.
pub fn rank_text(rank: u32) -> String {
    if rank == 0 {
        String::new()
    } else {
        format!("#{rank}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1000;

    #[test]
    fn no_hold_means_every_hold_flag_is_off() {
        assert!(!Holds::default().any());
        for h in [
            Holds {
                pointer: true,
                ..Holds::default()
            },
            Holds {
                focus: true,
                ..Holds::default()
            },
            Holds {
                dialog: true,
                ..Holds::default()
            },
            Holds {
                operation: true,
                ..Holds::default()
            },
        ] {
            assert!(h.any(), "{h:?}");
        }
    }

    #[test]
    fn nothing_waiting_never_applies() {
        let mut g = Gate::default();
        assert!(!g.poll(0, Holds::default()));
        assert!(!g.poll(100 * S, Holds::default()));
    }

    #[test]
    fn the_first_reorder_is_not_delayed() {
        let mut g = Gate::default();
        g.want();
        assert!(g.waiting());
        assert!(g.poll(5 * S, Holds::default()));
        assert!(!g.waiting());
        assert_eq!(g.age_s(5 * S), Some(0));
    }

    #[test]
    fn a_second_reorder_waits_for_thirty_seconds() {
        let mut g = Gate::default();
        g.want();
        assert!(g.poll(0, Holds::default()));
        g.want();
        assert!(!g.poll(10 * S, Holds::default()));
        assert!(!g.poll(MIN_INTERVAL_MS - 1, Holds::default()));
        assert!(g.waiting(), "still waiting, not dropped");
        assert!(g.poll(MIN_INTERVAL_MS, Holds::default()));
        assert!(!g.waiting());
    }

    #[test]
    fn each_hold_blocks_and_the_waiting_reorder_applies_after() {
        for hold in [
            Holds {
                pointer: true,
                ..Holds::default()
            },
            Holds {
                focus: true,
                ..Holds::default()
            },
            Holds {
                dialog: true,
                ..Holds::default()
            },
            Holds {
                operation: true,
                ..Holds::default()
            },
        ] {
            let mut g = Gate::default();
            g.want();
            assert!(!g.poll(0, hold), "first reorder under {hold:?}");
            assert!(!g.poll(300 * S, hold), "long after, still held: {hold:?}");
            assert!(g.waiting());
            assert!(g.poll(301 * S, Holds::default()), "after {hold:?}");
        }
    }

    #[test]
    fn force_restarts_the_interval_and_clears_the_wait() {
        let mut g = Gate::default();
        g.want();
        g.force(50 * S);
        assert!(!g.waiting());
        g.want();
        assert!(!g.poll(60 * S, Holds::default()));
        assert!(g.poll(80 * S, Holds::default()));
    }

    #[test]
    fn age_counts_whole_seconds_and_starts_empty() {
        let mut g = Gate::default();
        assert_eq!(g.age_s(0), None);
        g.force(10 * S);
        assert_eq!(g.age_s(10 * S), Some(0));
        assert_eq!(g.age_s(10 * S + 999), Some(0));
        assert_eq!(g.age_s(22 * S), Some(12));
        assert_eq!(g.age_s(5 * S), Some(0), "a clock step back is not negative");
    }

    #[test]
    fn sorted_by_text_names_the_order_and_its_age() {
        assert_eq!(
            sorted_by_text("30-min spend", Some(12)),
            "Sorted by 30-min spend - updated 12 s ago"
        );
        assert_eq!(
            sorted_by_text("memory", Some(0)),
            "Sorted by memory - updated 0 s ago"
        );
        assert_eq!(sorted_by_text("CPU", None), "Sorted by CPU");
    }

    #[test]
    fn the_selected_row_keeps_its_screen_position() {
        // Moved down 3 rows of 40 px: the view scrolls 120 px with it.
        assert_eq!(keep_position(200.0, 2, 5, 40.0), 320.0);
        // Moved up.
        assert_eq!(keep_position(200.0, 5, 1, 40.0), 40.0);
        // Same place: nothing moves.
        assert_eq!(keep_position(200.0, 4, 4, 40.0), 200.0);
        // Never scrolls above the top.
        assert_eq!(keep_position(20.0, 5, 0, 40.0), 0.0);
    }

    #[test]
    fn rank_is_one_based_with_a_hash() {
        assert_eq!(rank_text(1), "#1");
        assert_eq!(rank_text(40), "#40");
        assert_eq!(rank_text(0), "", "no rank before the first sort");
    }
}
