//! Gauge of open agent sessions, a cell of the machine strip (owner request 25/10): the number is
//! the open agent sessions, the arc is (sessions + live sub-sessions) over the
//! squad-watch ceiling, and a popover breaks the load down per tool and per
//! session. The counting is pure, so the numbers are tested without GTK.

use crate::format::sanitize;
use crate::memory_map::session_name;
use agentcenter_core::Snapshot;
use agentcenter_core::adapters::AgentRole;
use agentcenter_core::overlay::SessionOverlay;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// Sessions plus live sub-sessions at which squad-watch denies a dispatch.
pub const CEILING: u64 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warning,
    Error,
}

/// What one snapshot says about the load; equal counts mean nothing to redraw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    /// Open agent sessions; multiplexers are not counted.
    pub sessions: usize,
    /// Agent id and its sessions, most first, then by id.
    pub by_agent: Vec<(String, usize)>,
    /// Live sub-sessions; `None` while no session has reported them.
    pub live: Option<u64>,
    /// Sub-sessions spawned so far; `None` while none has reported them.
    pub spawned: Option<u64>,
    /// Display name and live sub-sessions of each session with at least one,
    /// most first.
    pub live_by_session: Vec<(String, u64)>,
}

impl Counts {
    pub fn of(snap: &Snapshot) -> Counts {
        let agents = || snap.sessions.iter().filter(|s| s.role == AgentRole::Agent);
        let mut by_agent: BTreeMap<&str, usize> = BTreeMap::new();
        for s in agents() {
            *by_agent.entry(s.agent.as_str()).or_default() += 1;
        }
        let mut by_agent: Vec<(String, usize)> = by_agent
            .into_iter()
            .map(|(a, n)| (a.to_string(), n))
            .collect();
        // Stable: ids are already in order, so ties stay by id.
        by_agent.sort_by_key(|a| std::cmp::Reverse(a.1));
        // Unknown adds nothing, and the sum is unknown only while no session
        // has reported (the summary's rule, over agent sessions only).
        let sum = |pick: fn(&SessionOverlay) -> Option<u64>| {
            agents()
                .filter_map(|s| s.overlay.as_ref().and_then(pick))
                .reduce(u64::saturating_add)
        };
        let mut live_by_session: Vec<(String, u64)> = agents()
            .filter_map(|s| {
                let n = s.overlay.as_ref()?.subagents_live.filter(|n| *n > 0)?;
                Some((session_name(s), n))
            })
            .collect();
        live_by_session.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Counts {
            sessions: agents().count(),
            by_agent,
            live: sum(|o| o.subagents_live),
            spawned: sum(|o| o.subagents_total),
            live_by_session,
        }
    }

    fn used(&self) -> u64 {
        self.sessions as u64 + self.live.unwrap_or(0)
    }

    /// Share of [`CEILING`] in use, 0..=1; an unknown live count adds nothing.
    pub fn fill(&self) -> f64 {
        (self.used() as f64 / CEILING as f64).min(1.0)
    }

    pub fn level(&self) -> Level {
        level_of(self.used())
    }

    /// The strip cell's sub line: `11 of 40`, `at least 7 of 40` while the live
    /// sub-sessions are unknown.
    pub fn slots(&self) -> String {
        let bound = if self.live.is_none() { "at least " } else { "" };
        format!("{bound}{} of {CEILING}", self.used())
    }

    /// The accessible label and tooltip.
    pub fn label(&self) -> String {
        let s = self.sessions;
        let sessions = format!("{s} open session{}", if s == 1 { "" } else { "s" });
        let live = match self.live {
            Some(n) => format!(
                "{sessions}, {n} live sub-session{}",
                if n == 1 { "" } else { "s" }
            ),
            None => format!("{sessions}, live sub-sessions unknown"),
        };
        // The ring's colour in words, for whoever cannot see it.
        let level = match self.level() {
            Level::Normal => "",
            Level::Warning => ", near the limit",
            Level::Error => ", at the limit",
        };
        // Unknown live sub-sessions make `used` a lower bound: say so.
        let bound = if self.live.is_none() { "at least " } else { "" };
        format!("{live}; {bound}{} of {CEILING}{level}", self.used())
    }
}

/// The level for `used` slots of [`CEILING`]: warning from 75 %, error from 90 %.
pub fn level_of(used: u64) -> Level {
    // Integer compare: 30 of 40 is exactly 75 %, with no float edge.
    if used * 100 >= 90 * CEILING {
        Level::Error
    } else if used * 100 >= 75 * CEILING {
        Level::Warning
    } else {
        Level::Normal
    }
}

/// How an agent id reads in the popover.
pub fn agent_title(id: &str) -> String {
    match id {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        other => sanitize(other).into_owned(),
    }
}

const DASH: &str = "\u{2013}";
const SIZE_PX: i32 = 28;
const STROKE_PX: f64 = 3.0;

pub(crate) fn level_class(l: Level) -> &'static str {
    match l {
        Level::Normal => "level-normal",
        Level::Warning => "level-warning",
        Level::Error => "level-error",
    }
}

/// The level ring: a track and an arc over `fill` (0..=1) in the widget's
/// colour, which the level classes set.
pub(crate) fn ring(fill: Rc<Cell<f64>>) -> gtk::DrawingArea {
    let arc = gtk::DrawingArea::builder()
        .content_width(SIZE_PX)
        .content_height(SIZE_PX)
        .build();
    arc.add_css_class("gauge-arc");
    arc.set_accessible_role(gtk::AccessibleRole::Presentation);
    arc.set_draw_func(move |area, cr, w, h| {
        let c = area.color();
        let (w, h) = (f64::from(w), f64::from(h));
        let r = (w.min(h) - STROKE_PX) / 2.0;
        cr.set_line_width(STROKE_PX);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        let rgba = |a: f64| {
            cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), a);
        };
        rgba(0.25);
        cr.arc(w / 2.0, h / 2.0, r, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
        let share = fill.get();
        if share > 0.0 {
            rgba(f64::from(c.alpha()));
            let start = -std::f64::consts::FRAC_PI_2;
            cr.arc(
                w / 2.0,
                h / 2.0,
                r,
                start,
                start + share * std::f64::consts::TAU,
            );
            let _ = cr.stroke();
        }
    });
    arc
}

/// The strip cell's button: a ring with the session count inside, and a popover
/// with the breakdown. A snapshot with the same counts touches nothing.
pub struct SessionGauge {
    pub button: gtk::MenuButton,
    number: gtk::Label,
    arc: gtk::DrawingArea,
    fill: Rc<Cell<f64>>,
    content: gtk::Box,
    last: RefCell<Option<Counts>>,
    /// What the tooltip and the accessible label last said.
    spoken: RefCell<String>,
}

impl SessionGauge {
    pub fn new() -> Self {
        let fill = Rc::new(Cell::new(0.0));
        let arc = ring(fill.clone());
        let number = gtk::Label::new(Some(DASH));
        number.add_css_class("gauge-number");
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&arc));
        overlay.add_overlay(&number);

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
            .child(&overlay)
            .popover(&popover)
            .valign(gtk::Align::Center)
            .tooltip_text("Open sessions")
            .build();
        button.add_css_class("flat");
        button.add_css_class("session-gauge");
        button.add_css_class(level_class(Level::Normal));
        let gauge = SessionGauge {
            button,
            number,
            arc,
            fill,
            content,
            last: RefCell::new(None),
            spoken: RefCell::default(),
        };
        gauge.announce("Open sessions: unknown");
        gauge
    }

    /// One string for the tooltip and the accessible label.
    fn announce(&self, label: &str) {
        self.button.set_tooltip_text(Some(label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.spoken.replace(label.to_string());
    }

    #[cfg(test)]
    fn spoken(&self) -> String {
        self.spoken.borrow().clone()
    }

    pub fn update(&self, snap: &Snapshot) {
        let counts = Counts::of(snap);
        if self.last.borrow().as_ref() == Some(&counts) {
            return;
        }
        let previous = self.last.replace(Some(counts.clone()));
        self.number.set_label(&counts.sessions.to_string());
        self.announce(&counts.label());
        let level = counts.level();
        if previous.as_ref().map(Counts::level) != Some(level) {
            for l in [Level::Normal, Level::Warning, Level::Error] {
                self.button.remove_css_class(level_class(l));
            }
            self.button.add_css_class(level_class(level));
        }
        self.fill.set(counts.fill());
        self.arc.queue_draw();
        self.rebuild(&counts);
    }

    fn rebuild(&self, c: &Counts) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let count = |n: Option<u64>| n.map_or_else(|| DASH.to_string(), |n| n.to_string());
        self.content.append(&heading("Sessions by tool"));
        for (agent, n) in &c.by_agent {
            self.content
                .append(&row(&agent_title(agent), &n.to_string()));
        }
        self.content.append(&heading("Sub-sessions"));
        self.content.append(&row("Live now", &count(c.live)));
        for (name, n) in &c.live_by_session {
            let r = row(name, &n.to_string());
            r.add_css_class("gauge-nested");
            self.content.append(&r);
        }
        self.content
            .append(&row("Spawned in total", &count(c.spawned)));
    }
}

pub(crate) fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("heading");
    l
}

pub(crate) fn row(name: &str, value: &str) -> gtk::Box {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    r.add_css_class("gauge-row");
    let n = gtk::Label::new(Some(name));
    n.set_xalign(0.0);
    n.set_hexpand(true);
    n.set_ellipsize(gtk::pango::EllipsizeMode::End);
    n.set_max_width_chars(32);
    // An ellipsized name stays readable on hover.
    n.set_tooltip_text(Some(name));
    n.add_css_class("gauge-name");
    let v = gtk::Label::new(Some(value));
    v.set_xalign(1.0);
    v.add_css_class("gauge-value");
    r.append(&n);
    r.append(&v);
    r
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

    /// Sets the sub-session counts of the session named `name`.
    fn subagents(v: &mut Value, name: &str, live: Value, total: Value) {
        let s = v["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["overlay"]["name"] == name)
            .unwrap();
        s["overlay"]["subagents_live"] = live;
        s["overlay"]["subagents_total"] = total;
    }

    #[test]
    fn sessions_exclude_multiplexers_and_group_by_agent() {
        let c = Counts::of(&cockpit(|_| {}));
        assert_eq!(c.sessions, 7, "8 sessions, one of them herdr");
        let want = [("claude", 3), ("pi", 2), ("codex", 1), ("gemini", 1)];
        assert_eq!(
            c.by_agent,
            want.map(|(a, n)| (a.to_string(), n)).to_vec(),
            "most first, ties by id"
        );
    }

    /// Card 4d96cc0b: the strip cell's sub line.
    #[test]
    fn slots_reads_n_of_40_and_is_a_lower_bound_while_live_is_unknown() {
        let mut c = Counts::of(&cockpit(|_| {}));
        assert_eq!(c.live, None);
        assert_eq!(c.slots(), "at least 7 of 40");
        c.live = Some(4);
        assert_eq!(c.slots(), "11 of 40");
        c.live = Some(0);
        assert_eq!(c.slots(), "7 of 40", "a known zero is not a bound");
    }

    #[test]
    fn unreported_sub_sessions_are_unknown_not_zero() {
        let c = Counts::of(&cockpit(|_| {}));
        assert_eq!((c.live, c.spawned), (None, None));
        assert!(c.live_by_session.is_empty());
        assert!((c.fill() - 7.0 / 40.0).abs() < 1e-9, "sessions alone");
    }

    #[test]
    fn live_sub_sessions_sum_and_list_highest_first() {
        let c = Counts::of(&cockpit(|v| {
            subagents(v, "quill-flaky-tests", json!(1), json!(2));
            subagents(v, "atlas-api-auth-refactor", json!(3), json!(5));
            subagents(v, "ledger-migration-v4", json!(0), json!(1));
            subagents(v, "relay-i18n", json!(null), json!(null));
        }));
        assert_eq!(c.live, Some(4));
        assert_eq!(c.spawned, Some(8));
        assert_eq!(
            c.live_by_session,
            vec![
                ("atlas-api-auth-refactor".to_string(), 3),
                ("quill-flaky-tests".to_string(), 1)
            ],
            "zero and unknown sessions stay out; the name is the list's"
        );
        assert!((c.fill() - 11.0 / 40.0).abs() < 1e-9);
    }

    #[test]
    fn a_known_zero_is_zero_and_stays_distinct_from_unknown() {
        let c = Counts::of(&cockpit(|v| {
            subagents(v, "relay-i18n", json!(0), json!(0));
        }));
        assert_eq!((c.live, c.spawned), (Some(0), Some(0)));
    }

    #[test]
    fn level_is_normal_below_75_warning_from_75_and_error_from_90() {
        assert_eq!(level_of(0), Level::Normal);
        assert_eq!(level_of(29), Level::Normal, "72.5 %");
        assert_eq!(level_of(30), Level::Warning, "75 %");
        assert_eq!(level_of(35), Level::Warning, "87.5 %");
        assert_eq!(level_of(36), Level::Error, "90 %");
        assert_eq!(level_of(200), Level::Error);
    }

    #[test]
    fn fill_stops_at_the_ceiling_and_level_follows_sessions_plus_live() {
        let c = Counts::of(&cockpit(|v| {
            subagents(v, "atlas-api-auth-refactor", json!(23), json!(23));
        }));
        assert_eq!(c.level(), Level::Warning, "7 + 23 = 30 of 40");
        let c = Counts::of(&cockpit(|v| {
            subagents(v, "atlas-api-auth-refactor", json!(90), json!(90));
        }));
        assert_eq!((c.fill(), c.level()), (1.0, Level::Error));
    }

    #[test]
    fn label_states_the_counts_and_says_unknown() {
        let c = Counts::of(&cockpit(|v| {
            subagents(v, "atlas-api-auth-refactor", json!(4), json!(9));
        }));
        assert_eq!(c.label(), "7 open sessions, 4 live sub-sessions; 11 of 40");
        let unknown = Counts::of(&cockpit(|_| {}));
        assert_eq!(
            unknown.label(),
            "7 open sessions, live sub-sessions unknown; at least 7 of 40"
        );
        let one = Counts::of(&cockpit(|v| {
            v["sessions"].as_array_mut().unwrap().truncate(1);
            subagents(v, "relay-i18n", json!(1), json!(1));
        }));
        assert_eq!(one.label(), "1 open session, 1 live sub-session; 2 of 40");
    }

    #[test]
    fn agent_titles_name_the_known_tools_and_pass_others_through() {
        assert_eq!(agent_title("claude"), "Claude");
        assert_eq!(agent_title("codex"), "Codex");
        assert_eq!(agent_title("pi"), "pi");
        assert_eq!(agent_title("gemini"), "gemini");
    }

    #[test]
    fn the_label_says_the_level_in_words() {
        let mut c = Counts::of(&cockpit(|v| {
            subagents(v, "atlas-api-auth-refactor", json!(23), json!(30));
        }));
        assert_eq!(c.level(), Level::Warning);
        assert!(
            c.label().ends_with("; 30 of 40, near the limit"),
            "{}",
            c.label()
        );
        c.live = Some(33);
        assert_eq!(c.level(), Level::Error);
        assert!(
            c.label().ends_with("; 40 of 40, at the limit"),
            "{}",
            c.label()
        );
    }

    /// The accessible label is set at creation and follows every update.
    #[test]
    fn the_accessible_label_follows_the_counts() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let gauge = SessionGauge::new();
            assert!(gtk::test_accessible_has_property(
                &gauge.button,
                gtk::AccessibleProperty::Label
            ));
            assert_eq!(gauge.spoken(), "Open sessions: unknown");
            let snap = cockpit(|v| {
                subagents(v, "atlas-api-auth-refactor", json!(4), json!(9));
            });
            gauge.update(&snap);
            assert_eq!(gauge.spoken(), Counts::of(&snap).label());
            assert_eq!(
                gauge.button.tooltip_text().as_deref(),
                Some(gauge.spoken().as_str())
            );
        });
    }
}
