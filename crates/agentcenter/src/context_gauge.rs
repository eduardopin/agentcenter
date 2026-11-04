//! Context gauge, a cell of the machine strip (owner request 25/10): the
//! fullest open agent session's context %, with that session's name beside it,
//! and a popover listing the sessions at or above the squad-watch warning. The
//! % is the overlay's `context_pct`, the field the session list column reads,
//! formatted the same way. The reading is pure, so the numbers are tested
//! without GTK.

use crate::format::ABSENT;
use crate::memory_map::session_name;
use crate::session_gauge::{Level, heading, level_class, ring, row};
use agentcenter_core::Snapshot;
use agentcenter_core::adapters::AgentRole;
use agentcenter_core::cockpit::Recommendation;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Context % from which squad-watch warns.
pub const WARN_PCT: f64 = 75.0;
/// Context % from which squad-watch orders a handoff.
pub const ERROR_PCT: f64 = 90.0;

/// One session at or above the warning, as the popover lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Whole percent, as the list column prints it.
    pub pct: f64,
    /// The cockpit's verdict when it is Compact or Handoff.
    pub verdict: Option<&'static str>,
}

/// What one snapshot says about context; an equal reading means nothing to redraw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    /// Open agent sessions; multiplexers are not counted.
    pub sessions: usize,
    /// The fullest session; `None` while no session reports a context.
    pub worst: Option<Entry>,
    /// Sessions at or above [`WARN_PCT`], highest first, then by name.
    pub high: Vec<Entry>,
    /// Sessions at or above [`ERROR_PCT`].
    pub critical: usize,
}

impl Reading {
    pub fn of(snap: &Snapshot) -> Reading {
        let agents = || snap.sessions.iter().filter(|s| s.role == AgentRole::Agent);
        let mut known: Vec<Entry> = agents()
            .filter_map(|s| {
                let o = s.overlay.as_ref()?;
                // The list column's rule: a finite value, never below 0.
                let pct = o.context_pct.filter(|c| c.is_finite())?.max(0.0);
                let verdict = match o.cockpit.as_ref().map(|c| c.recommendation) {
                    Some(Recommendation::Compactar) => Some("Compact"),
                    Some(Recommendation::Handoff) => Some("Handoff"),
                    _ => None,
                };
                Some(Entry {
                    name: session_name(s),
                    pct: whole(pct),
                    verdict,
                })
            })
            .collect();
        known.sort_by(|a, b| b.pct.total_cmp(&a.pct).then_with(|| a.name.cmp(&b.name)));
        Reading {
            sessions: agents().count(),
            worst: known.first().cloned(),
            critical: known.iter().filter(|e| e.pct >= ERROR_PCT).count(),
            high: known.into_iter().filter(|e| e.pct >= WARN_PCT).collect(),
        }
    }

    pub fn level(&self) -> Level {
        match &self.worst {
            Some(w) if w.pct >= ERROR_PCT => Level::Error,
            Some(w) if w.pct >= WARN_PCT => Level::Warning,
            _ => Level::Normal,
        }
    }

    /// The big readout: `96%`, a dash while unknown (never 0).
    pub fn value(&self) -> String {
        self.worst
            .as_ref()
            .map_or_else(|| ABSENT.to_owned(), |w| percent(w.pct))
    }

    /// The line under it: the fullest session's name, or why there is none.
    pub fn sub(&self) -> String {
        match (&self.worst, self.sessions) {
            (Some(w), _) => w.name.clone(),
            (None, 0) => "no open sessions".to_owned(),
            (None, _) => "no context reading".to_owned(),
        }
    }

    /// Ring fill, 0..=1.
    pub fn fill(&self) -> f64 {
        self.worst
            .as_ref()
            .map_or(0.0, |w| (w.pct / 100.0).min(1.0))
    }

    /// The accessible label and tooltip.
    pub fn label(&self) -> String {
        let Some(w) = &self.worst else {
            return match self.sessions {
                0 => "Context: no open sessions".to_owned(),
                _ => "Context: unknown, no open session reports it".to_owned(),
            };
        };
        // The ring's colour in words, for whoever cannot see it.
        let level = match self.level() {
            Level::Normal => String::new(),
            Level::Warning => format!(", high ({WARN_PCT:.0}% or more)"),
            Level::Error => format!(", critical ({ERROR_PCT:.0}% or more)"),
        };
        let n = self.high.len();
        format!(
            "Context: highest {} in {}{level}; {n} session{} at or above {WARN_PCT:.0}%, {} at or above {ERROR_PCT:.0}%",
            percent(w.pct),
            w.name,
            if n == 1 { "" } else { "s" },
            self.critical,
        )
    }
}

/// The percent as the list column prints it, rounded to a whole number; the
/// levels compare this, so the cell never says 90% in the colour of 89%.
fn whole(pct: f64) -> f64 {
    format!("{pct:.0}").parse().unwrap_or(pct)
}

fn percent(pct: f64) -> String {
    format!("{pct:.0}%")
}

/// The strip cell: title, readout and name beside a ring button whose popover
/// lists the sessions at or above the warning.
pub struct ContextGauge {
    pub root: gtk::Grid,
    pub button: gtk::MenuButton,
    value: gtk::Label,
    name: gtk::Label,
    arc: gtk::DrawingArea,
    fill: Rc<Cell<f64>>,
    content: gtk::Box,
    last: RefCell<Option<Reading>>,
    /// What the tooltip and the accessible label last said.
    spoken: RefCell<String>,
    /// Popover rebuilds so far.
    rebuilds: Cell<u32>,
}

impl ContextGauge {
    pub fn new() -> Self {
        let root = gtk::Grid::new();
        root.set_column_spacing(8);
        root.add_css_class("cell");
        root.add_css_class("last");
        root.add_css_class("ring-cell");
        root.set_hexpand(true);
        let value = gtk::Label::new(Some(ABSENT));
        value.add_css_class("value");
        value.set_xalign(0.0);
        let name = gtk::Label::new(None);
        name.add_css_class("sub");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(24);
        let title = gtk::Label::new(Some("Context"));
        title.add_css_class("title");
        title.set_xalign(0.0);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        // Title and readout share a line, so the cell is two lines tall and
        // stacks over the sessions cell without growing the strip.
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        head.append(&title);
        head.append(&value);
        text.append(&head);
        text.append(&name);

        let fill = Rc::new(Cell::new(0.0));
        let arc = ring(fill.clone());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.add_css_class("gauge-popover");
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .child(&content)
            .build();
        let popover = gtk::Popover::builder().child(&scroll).build();
        let button = gtk::MenuButton::builder()
            .child(&arc)
            .popover(&popover)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.add_css_class("context-gauge");
        button.add_css_class(level_class(Level::Normal));
        root.attach(&text, 0, 0, 1, 1);
        root.attach(&button, 1, 0, 1, 1);
        let gauge = ContextGauge {
            root,
            button,
            value,
            name,
            arc,
            fill,
            content,
            last: RefCell::new(None),
            spoken: RefCell::default(),
            rebuilds: Cell::new(0),
        };
        gauge.announce("Context: unknown");
        gauge
    }

    /// One string for the tooltip and the accessible label.
    fn announce(&self, label: &str) {
        self.button.set_tooltip_text(Some(label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.spoken.replace(label.to_string());
    }

    pub fn update(&self, snap: &Snapshot) {
        let reading = Reading::of(snap);
        if self.last.borrow().as_ref() == Some(&reading) {
            return;
        }
        let previous = self.last.replace(Some(reading.clone()));
        self.value.set_label(&reading.value());
        self.name.set_label(&reading.sub());
        self.announce(&reading.label());
        let level = reading.level();
        if previous.as_ref().map(Reading::level) != Some(level) {
            for l in [Level::Normal, Level::Warning, Level::Error] {
                self.button.remove_css_class(level_class(l));
            }
            self.button.add_css_class(level_class(level));
        }
        self.fill.set(reading.fill());
        self.arc.queue_draw();
        self.rebuild(&reading);
    }

    fn rebuild(&self, r: &Reading) {
        self.rebuilds.set(self.rebuilds.get() + 1);
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        self.content
            .append(&heading(&format!("Sessions at or above {WARN_PCT:.0}%")));
        if r.high.is_empty() {
            self.content.append(&heading("None"));
        }
        for e in &r.high {
            let value = match e.verdict {
                Some(v) => format!("{} \u{b7} {v}", percent(e.pct)),
                None => percent(e.pct),
            };
            self.content.append(&row(&e.name, &value));
        }
        self.content.append(&row(
            &format!("At or above {WARN_PCT:.0}%"),
            &r.high.len().to_string(),
        ));
        self.content.append(&row(
            &format!("At or above {ERROR_PCT:.0}%"),
            &r.critical.to_string(),
        ));
    }

    #[cfg(test)]
    pub fn spoken(&self) -> String {
        self.spoken.borrow().clone()
    }

    #[cfg(test)]
    pub fn rebuilds(&self) -> u32 {
        self.rebuilds.get()
    }

    #[cfg(test)]
    pub fn value_label(&self) -> &gtk::Label {
        &self.value
    }

    #[cfg(test)]
    pub fn name_label(&self) -> &gtk::Label {
        &self.name
    }

    #[cfg(test)]
    pub fn content(&self) -> &gtk::Box {
        &self.content
    }

    #[cfg(test)]
    pub fn fill_share(&self) -> f64 {
        self.fill.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn cockpit(edit: impl FnOnce(&mut Value)) -> Snapshot {
        let mut v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/cockpit.json")).unwrap();
        edit(&mut v);
        serde_json::from_value(v).unwrap()
    }

    /// Sets the context % of the session named `name` (null clears it).
    fn ctx(v: &mut Value, name: &str, pct: Value) {
        let s = v["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["overlay"]["name"] == name)
            .unwrap();
        s["overlay"]["context_pct"] = pct;
    }

    fn all_unknown(v: &mut Value) {
        for s in v["sessions"].as_array_mut().unwrap() {
            s["overlay"]["context_pct"] = Value::Null;
        }
    }

    #[test]
    fn the_worst_is_the_highest_context_among_agent_sessions() {
        let r = Reading::of(&cockpit(|_| {}));
        let w = r.worst.as_ref().expect("a session has a context");
        assert_eq!((w.name.as_str(), w.pct), ("orbit-web-perf-pass", 77.0));
        assert_eq!(r.sessions, 7, "8 sessions, one of them herdr");
        assert_eq!(r.value(), "77%", "the list column's format");
        assert_eq!(r.sub(), "orbit-web-perf-pass");
    }

    #[test]
    fn a_multiplexer_never_counts_even_with_a_context() {
        let r = Reading::of(&cockpit(|v| {
            let mux = v["sessions"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|s| s["role"] == "multiplexer")
                .unwrap();
            mux["overlay"]["context_pct"] = json!(99.0);
        }));
        assert_eq!(r.worst.unwrap().pct, 77.0);
    }

    #[test]
    fn unknown_context_is_a_dash_never_zero() {
        let r = Reading::of(&cockpit(all_unknown));
        assert_eq!(r.worst, None);
        assert_eq!(
            (r.value().as_str(), r.sub().as_str()),
            (ABSENT, "no context reading")
        );
        assert_eq!(r.fill(), 0.0);
        assert_eq!(r.level(), Level::Normal);
        assert!(r.label().contains("unknown"), "{}", r.label());
        // One known session wins over the unknown ones.
        let r = Reading::of(&cockpit(|v| {
            all_unknown(v);
            ctx(v, "quill-flaky-tests", json!(35.0));
        }));
        assert_eq!(r.value(), "35%");
    }

    /// The list column's rule: a negative reading prints 0 %, never "-5%".
    #[test]
    fn a_negative_context_clamps_to_zero() {
        let r = Reading::of(&cockpit(|v| {
            all_unknown(v);
            ctx(v, "quill-flaky-tests", json!(-5.0));
        }));
        assert_eq!(r.value(), "0%");
    }

    #[test]
    fn no_sessions_is_a_dash_and_says_so() {
        let r = Reading::of(&cockpit(|v| v["sessions"] = json!([])));
        assert_eq!(r.sessions, 0);
        assert_eq!(
            (r.value().as_str(), r.sub().as_str()),
            (ABSENT, "no open sessions")
        );
        assert!(r.label().contains("no open sessions"), "{}", r.label());
    }

    #[test]
    fn levels_follow_the_whole_percent_the_cell_prints() {
        let level = |p: f64| {
            Reading::of(&cockpit(|v| {
                all_unknown(v);
                ctx(v, "quill-flaky-tests", json!(p));
            }))
            .level()
        };
        assert_eq!(level(74.4), Level::Normal, "prints 74%");
        assert_eq!(level(75.0), Level::Warning);
        assert_eq!(level(89.4), Level::Warning, "prints 89%");
        assert_eq!(level(89.6), Level::Error, "prints 90%");
        assert_eq!(level(90.0), Level::Error);
        assert_eq!(level(250.0), Level::Error);
    }

    #[test]
    fn the_list_is_at_or_above_75_highest_first_with_the_verdict_and_the_counts() {
        let r = Reading::of(&cockpit(|v| {
            ctx(v, "relay-i18n", json!(75.0));
            ctx(v, "ledger-migration-v4", json!(93.0)); // COMPACTAR
            ctx(v, "harbor-docs", json!(74.0));
            ctx(v, "atlas-api-auth-refactor", json!(90.0)); // CONTINUAR: no verdict
        }));
        let got: Vec<_> = r
            .high
            .iter()
            .map(|e| (e.name.as_str(), e.pct, e.verdict))
            .collect();
        assert_eq!(
            got,
            [
                ("ledger-migration-v4", 93.0, Some("Compact")),
                ("atlas-api-auth-refactor", 90.0, None),
                ("orbit-web-perf-pass", 77.0, Some("Handoff")),
                ("relay-i18n", 75.0, None),
            ]
        );
        assert_eq!(r.critical, 2, "93 and 90");
        assert_eq!(r.worst.unwrap().name, "ledger-migration-v4");
    }

    #[test]
    fn ties_go_by_name() {
        let r = Reading::of(&cockpit(|v| {
            ctx(v, "relay-i18n", json!(80.0));
            ctx(v, "harbor-docs", json!(80.0));
            ctx(v, "orbit-web-perf-pass", json!(10.0));
        }));
        let names: Vec<_> = r.high.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["harbor-docs", "relay-i18n"]);
        assert_eq!(r.worst.unwrap().name, "harbor-docs");
    }

    #[test]
    fn fill_is_the_percent_and_stops_at_the_full_ring() {
        let r = Reading::of(&cockpit(|_| {}));
        assert!((r.fill() - 0.77).abs() < 1e-9);
        let r = Reading::of(&cockpit(|v| ctx(v, "relay-i18n", json!(180.0))));
        assert_eq!(r.fill(), 1.0);
    }

    #[test]
    fn the_label_states_the_level_in_words_and_the_counts() {
        let normal = Reading::of(&cockpit(|v| ctx(v, "orbit-web-perf-pass", json!(50.0))));
        assert_eq!(
            normal.label(),
            "Context: highest 62% in ledger-migration-v4; 0 sessions at or above 75%, 0 at or above 90%"
        );
        let warn = Reading::of(&cockpit(|_| {}));
        assert_eq!(
            warn.label(),
            "Context: highest 77% in orbit-web-perf-pass, high (75% or more); \
             1 session at or above 75%, 0 at or above 90%"
        );
        let err = Reading::of(&cockpit(|v| ctx(v, "relay-i18n", json!(96.4))));
        assert_eq!(
            err.label(),
            "Context: highest 96% in relay-i18n, critical (90% or more); \
             2 sessions at or above 75%, 1 at or above 90%"
        );
    }

    #[test]
    fn the_popover_lists_the_sessions_and_the_counts() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let gauge = ContextGauge::new();
            gauge.update(&cockpit(|v| ctx(v, "ledger-migration-v4", json!(93.0))));
            let rows: Vec<(String, String)> = {
                let mut out = vec![];
                let mut child = gauge.content().first_child();
                while let Some(w) = child {
                    if w.has_css_class("gauge-row") {
                        let labels: Vec<String> = {
                            let mut l = vec![];
                            let mut c = w.first_child();
                            while let Some(x) = c {
                                if let Some(x) = x.downcast_ref::<gtk::Label>() {
                                    l.push(x.label().to_string());
                                }
                                c = x.next_sibling();
                            }
                            l
                        };
                        out.push((labels[0].clone(), labels[1].clone()));
                    }
                    child = w.next_sibling();
                }
                out
            };
            let want = |a: &str, b: &str| (a.to_string(), b.to_string());
            assert_eq!(
                rows,
                [
                    want("ledger-migration-v4", "93% \u{b7} Compact"),
                    want("orbit-web-perf-pass", "77% \u{b7} Handoff"),
                    want("At or above 75%", "2"),
                    want("At or above 90%", "1"),
                ]
            );
        });
    }

    /// The tooltip and the accessible label are one string, set at creation and
    /// after every change; the level class follows the reading.
    #[test]
    fn the_accessible_label_follows_the_reading_and_the_level_class_too() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let gauge = ContextGauge::new();
            assert!(gtk::test_accessible_has_property(
                &gauge.button,
                gtk::AccessibleProperty::Label
            ));
            assert_eq!(gauge.spoken(), "Context: unknown");
            let snap = cockpit(|_| {});
            gauge.update(&snap);
            assert_eq!(gauge.spoken(), Reading::of(&snap).label());
            assert_eq!(
                gauge.button.tooltip_text().as_deref(),
                Some(gauge.spoken().as_str())
            );
            assert!(gauge.button.has_css_class("level-warning"));
            assert_eq!(gauge.value_label().label(), "77%");
            assert_eq!(gauge.name_label().label(), "orbit-web-perf-pass");
            assert!((gauge.fill_share() - 0.77).abs() < 1e-9);
            gauge.update(&cockpit(|v| ctx(v, "relay-i18n", json!(96.0))));
            assert!(gauge.button.has_css_class("level-error"));
            assert!(!gauge.button.has_css_class("level-warning"));
            gauge.update(&cockpit(|v| ctx(v, "orbit-web-perf-pass", json!(10.0))));
            assert!(gauge.button.has_css_class("level-normal"));
            assert!(!gauge.button.has_css_class("level-error"));
        });
    }

    #[test]
    fn an_equal_reading_rebuilds_nothing() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let gauge = ContextGauge::new();
            let snap = cockpit(|_| {});
            gauge.update(&snap);
            assert_eq!(gauge.rebuilds(), 1);
            let mut later = snap.clone();
            later.taken_at_ms += 1000; // a new frame, the same context
            gauge.update(&later);
            assert_eq!(gauge.rebuilds(), 1, "same reading: popover untouched");
            gauge.update(&cockpit(|v| ctx(v, "relay-i18n", json!(80.0))));
            assert_eq!(gauge.rebuilds(), 2, "a changed reading rebuilds");
        });
    }
}
