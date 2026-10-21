//! Session-wide history, observed in the UI only; no synthetic prehistory.
use crate::graph::{Strip, nice_max, peak};
use adw::prelude::*;
use agentcenter_core::{Snapshot, session::Metric};
use std::cell::RefCell;
use std::collections::VecDeque;

const CAP: usize = 600;

#[derive(Clone)]
struct Record {
    at: u64,
    raw: [Option<f64>; 3],
    partial: [bool; 3],
    values: [f32; 3],
}

impl Record {
    fn from_snapshot(snap: &Snapshot) -> Self {
        let metrics = [Metric::CpuPct, Metric::PssBytes, Metric::SwapPssBytes];
        let raw = [
            snap.summary.sessions_cpu_pct,
            snap.summary
                .sessions_pss_bytes
                .map(|v| v as f64 / 1048576.0),
            snap.summary
                .sessions_swap_pss_bytes
                .map(|v| v as f64 / 1048576.0),
        ];
        let partial: [bool; 3] = std::array::from_fn(|i| {
            snap.sessions.iter().any(|s| {
                let missing = match i {
                    0 => s.cpu_pct.is_none(),
                    1 => s.pss_bytes.is_none(),
                    _ => s.swap_pss_bytes.is_none(),
                };
                missing || s.partial.contains(&metrics[i])
            })
        });
        let values = std::array::from_fn(|i| {
            if partial[i] {
                f32::NAN
            } else {
                raw[i]
                    .filter(|v| v.is_finite())
                    .map_or(f32::NAN, |v| v as f32)
            }
        });
        Self {
            at: snap.taken_at_ms,
            raw,
            partial,
            values,
        }
    }
}

/// The same widget-free model is used by the dashboard and its history tests.
#[derive(Default)]
struct ObservedHistory {
    samples: VecDeque<Record>,
}

struct HistoryView {
    timeline: String,
    slots: usize,
    series: [Vec<f32>; 3],
    scales: [f64; 3],
}

impl ObservedHistory {
    /// Returns false for a duplicate timestamp, leaving the observation unchanged.
    fn record(&mut self, snap: &Snapshot) -> bool {
        let at = snap.taken_at_ms;
        if self.samples.back().is_some_and(|s| at < s.at) {
            self.samples.clear(); // Clock reset: never connect unrelated timelines.
        }
        if self.samples.back().is_some_and(|s| at == s.at) {
            return false;
        }
        self.samples.push_back(Record::from_snapshot(snap));
        while self.samples.len() > CAP
            || self
                .samples
                .front()
                .is_some_and(|s| at.saturating_sub(s.at) >= CAP as u64 * 1000)
        {
            self.samples.pop_front();
        }
        true
    }

    fn view(&self) -> Option<HistoryView> {
        let latest = self.samples.back()?;
        let first = self.samples.front()?.at;
        // One slot per observed second. Unobserved slots remain NaN; no interpolation.
        let slots = (((latest.at - first) / 1000) as usize + 1).clamp(2, CAP);
        let mut series = [
            vec![f32::NAN; slots],
            vec![f32::NAN; slots],
            vec![f32::NAN; slots],
        ];
        for sample in &self.samples {
            let age = ((latest.at - sample.at) / 1000) as usize;
            if age < slots {
                for (i, row) in series.iter_mut().enumerate() {
                    row[slots - 1 - age] = sample.values[i];
                }
            }
        }
        let scales = std::array::from_fn(|i| {
            nice_max(
                peak([series[i].as_slice()]).unwrap_or(0.0),
                if i == 0 { 100.0 } else { 1.0 },
                i != 0,
            )
        });
        Some(HistoryView {
            timeline: format!(
                "Observed {}–{} · UTC · up to 10 min · 1 s slots",
                clock(first),
                clock(latest.at)
            ),
            slots,
            series,
            scales,
        })
    }
}

pub struct Dashboard {
    pub widget: gtk::Box,
    timeline: gtk::Label,
    rows: Vec<(gtk::Label, Strip)>,
    history: RefCell<ObservedHistory>,
}

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build()
}

impl Dashboard {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);
        widget.add_css_class("session-dashboard");
        let heading = label("Session resource history");
        heading.add_css_class("title-3");
        heading.set_accessible_role(gtk::AccessibleRole::Heading);
        widget.append(&heading);
        let timeline = label("Waiting for the first snapshot…");
        timeline.add_css_class("history-source");
        widget.append(&timeline);
        let rows = ["CPU · % of one core", "PSS · MiB", "SwapPss · MiB"]
            .into_iter()
            .map(|name| {
                let readout = label(name);
                readout.add_css_class("history-readout");
                let strip = Strip::new(1);
                strip.set_height_request(56);
                widget.append(&readout);
                widget.append(&strip);
                (readout, strip)
            })
            .collect();
        let source = label(
            "Session processes · /proc stat and smaps_rollup. Amber: complete sum; gaps: missing or partial readings. History starts when AgentCenter observes it. Select a session for its processes and transcript.",
        );
        source.add_css_class("history-source");
        widget.append(&source);
        Self {
            widget,
            timeline,
            rows,
            history: RefCell::new(ObservedHistory::default()),
        }
    }

    pub fn apply(&self, snap: &Snapshot) {
        let mut history = self.history.borrow_mut();
        if !history.record(snap) {
            return;
        }
        let view = history.view().expect("recorded observation");
        let latest = history.samples.back().expect("recorded observation");
        let raw = latest.raw;
        let partial = latest.partial;
        let HistoryView {
            timeline,
            slots,
            series,
            scales,
        } = view;
        if self.timeline.text() != timeline {
            self.timeline.set_text(&timeline);
        }
        for (i, (readout, strip)) in self.rows.iter().enumerate() {
            let (name, unit) = [("CPU", "% of one core"), ("PSS", "MiB"), ("SwapPss", "MiB")][i];
            let current = raw[i].map_or_else(
                || "unavailable".into(),
                |v| {
                    format!(
                        "{v:.1} {unit}{}",
                        if partial[i] {
                            " · partial sum, not plotted"
                        } else {
                            " · complete sum"
                        }
                    )
                },
            );
            let max = scales[i];
            let text = format!("{name}  {current} · scale 0–{max:.0} {unit}");
            if readout.text() != text {
                readout.set_text(&text);
            }
            strip.set(&[&series[i]], max, slots, None);
            let spoken = format!(
                "{text}. {timeline}. Missing or partial readings break the line. Source: session processes, /proc stat and smaps_rollup."
            );
            strip.update_property(&[gtk::accessible::Property::Label(&spoken)]);
        }
    }
}

fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::{IoStallSource, Machine, Session, adapters::AgentRole};

    fn snap(at: u64) -> Snapshot {
        Snapshot {
            schema_version: agentcenter_core::snapshot::SCHEMA_VERSION,
            taken_at_ms: at,
            interval_ms: 1000,
            machine: Machine {
                cores: 16,
                cpu_pct: None,
                io_stall_source: IoStallSource::Delayacct,
                processes: 0,
                session_processes: 0,
                mem_total_bytes: None,
                mem_available_bytes: None,
                swap_total_bytes: None,
                swap_free_bytes: None,
                load_avg: None,
                psi: None,
            },
            sessions: vec![],
            summary: Default::default(),
        }
    }

    /// Missing telemetry must never become a measured zero, including an empty rack.
    #[test]
    fn empty_and_zero_are_distinct() {
        assert!(ObservedHistory::default().view().is_none());
        let mut s = snap(0);
        assert!(Record::from_snapshot(&s).values.iter().all(|v| v.is_nan()));
        s.summary.sessions_cpu_pct = Some(0.0);
        s.summary.sessions_pss_bytes = Some(0);
        s.summary.sessions_swap_pss_bytes = Some(0);
        let r = Record::from_snapshot(&s);
        assert_eq!(r.values, [0.0; 3]);
        assert_eq!(r.partial, [false; 3]);
    }

    /// A partial sum remains available as text but must break only its own graph.
    #[test]
    fn partial_and_missing_session_metrics_are_not_complete_totals() {
        let mut s = snap(1000);
        s.summary.sessions_cpu_pct = Some(12.0);
        s.summary.sessions_pss_bytes = Some(3 << 20);
        s.summary.sessions_swap_pss_bytes = Some(2 << 20);
        let mut session = Session::aggregate("pi".into(), AgentRole::Agent, 7, 16, vec![]);
        session.cpu_pct = Some(12.0);
        session.pss_bytes = Some(3 << 20);
        session.swap_pss_bytes = Some(2 << 20);
        session.partial.clear();
        s.sessions.push(session);
        for i in 0..3 {
            let mut partial = s.clone();
            partial.sessions[0]
                .partial
                .push([Metric::CpuPct, Metric::PssBytes, Metric::SwapPssBytes][i]);
            let r = Record::from_snapshot(&partial);
            assert!(r.partial[i] && r.values[i].is_nan());
            assert!(r.raw[i].is_some());
            for j in 0..3 {
                if i != j {
                    assert!(r.values[j].is_finite());
                }
            }
        }
        s.sessions[0].pss_bytes = None;
        let r = Record::from_snapshot(&s);
        assert!(r.partial[1] && r.values[1].is_nan());
        assert_eq!(r.raw[1], Some(3.0));
    }

    /// Duplicate updates cannot rewrite a tick; a backwards clock starts a new history.
    #[test]
    fn duplicate_and_backwards_timestamps() {
        let mut h = ObservedHistory::default();
        let mut s = snap(2000);
        s.summary.sessions_cpu_pct = Some(1.0);
        assert!(h.record(&s));
        s.summary.sessions_cpu_pct = Some(99.0);
        assert!(!h.record(&s));
        assert_eq!(h.samples.len(), 1);
        assert_eq!(h.samples[0].raw[0], Some(1.0));
        s.taken_at_ms = 1000;
        assert!(h.record(&s));
        assert_eq!(h.samples.len(), 1);
        assert_eq!(h.samples[0].at, 1000);
    }

    /// Two-second cadence leaves gaps; 500 ms observations must not invent extra seconds.
    #[test]
    fn actual_time_slots_and_subsecond_collision() {
        let mut h = ObservedHistory::default();
        for (at, cpu) in [(0, 1.0), (2000, 2.0)] {
            let mut s = snap(at);
            s.summary.sessions_cpu_pct = Some(cpu);
            h.record(&s);
        }
        let v = h.view().unwrap();
        assert_eq!(v.slots, 3);
        assert_eq!((v.series[0][0], v.series[0][2]), (1.0, 2.0));
        assert!(v.series[0][1].is_nan());
        let mut s = snap(2500);
        s.summary.sessions_cpu_pct = Some(3.0);
        h.record(&s);
        let v = h.view().unwrap();
        assert_eq!(v.slots, 3);
        assert_eq!(v.series[0][2], 3.0);
        assert!(v.timeline.contains("UTC"));
    }

    /// Both the 600-observation cap and ten-minute expiry apply independently of cadence.
    #[test]
    fn bounded_capacity_and_ten_minute_window() {
        let mut h = ObservedHistory::default();
        for i in 0..1201 {
            h.record(&snap(i * 500));
        }
        assert_eq!(h.samples.len(), 600);
        assert_eq!(h.samples.front().unwrap().at, 300500);
        assert!(h.view().unwrap().slots <= 600);
        let mut h = ObservedHistory::default();
        for i in 0..301 {
            h.record(&snap(i * 2000));
        }
        assert_eq!(h.samples.len(), 300);
        assert_eq!(h.samples.front().unwrap().at, 2000);
        h.record(&snap(1_300_000));
        assert_eq!(h.samples.len(), 1);
        assert_eq!(h.view().unwrap().slots, 2);
    }

    /// Memory uses MiB, CPU can exceed one core, and peaks must fit the scale.
    #[test]
    fn mib_units_peak_scales_and_nonfinite_gaps() {
        let mut h = ObservedHistory::default();
        let mut s = snap(0);
        s.summary.sessions_cpu_pct = Some(320.0);
        s.summary.sessions_pss_bytes = Some(3 << 20);
        s.summary.sessions_swap_pss_bytes = Some(9 << 20);
        h.record(&s);
        let v = h.view().unwrap();
        assert_eq!(v.scales, [500.0, 4.0, 16.0]);
        assert_eq!(h.samples[0].raw, [Some(320.0), Some(3.0), Some(9.0)]);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            s.summary.sessions_cpu_pct = Some(value);
            assert!(Record::from_snapshot(&s).values[0].is_nan());
        }
    }
}
