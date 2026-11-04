//! Tile motion for the maps: the spring of the AgEnFK board's cards.
//!
//! The board moves a card with framer-motion `layout: "position"` and the
//! transition `{type: "spring", stiffness: 260, damping: 30, mass: 1}`: a
//! damping ratio of about 0.93, settled in about 0.4 s. [`BOARD`] holds those
//! numbers.
//!
//! - [`spring_step`]: one step of a damped spring, pure.
//! - [`Motion`]: a spring per tile for x, y, width and height, plus an
//!   opacity spring for a tile that enters or leaves. Pure, no GTK; keyed by
//!   whatever identifies a tile.
//! - [`TileMotion`]: the GTK side. It owns a [`Motion`] for one map, reads
//!   `gtk-enable-animations`, and runs a tick callback only while a spring
//!   is moving; once everything is settled no callback is left.

use crate::treemap::Rect;
use gtk::{glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// A spring's constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
}

/// What the board's cards use.
pub const BOARD: Params = Params {
    stiffness: 260.0,
    damping: 30.0,
    mass: 1.0,
};

/// Position and speed of one value (px, px/s; opacity for the fade).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spring {
    pub pos: f64,
    pub vel: f64,
}

/// Inside this distance and under this speed a spring counts as arrived
/// (framer-motion rests at 0.01 and 2; a tile moves in whole px).
const REST_DIST: f64 = 0.1;
const REST_SPEED: f64 = 2.0;
/// Integration step: semi-implicit Euler is stable far above this for the
/// board's constants (angular frequency 16 rad/s).
const SUBSTEP_S: f64 = 1.0 / 240.0;
/// A stalled frame clock never throws a tile across the map.
const MAX_DT_S: f64 = 0.1;

/// The spring after `dt` seconds, pulled to `target`.
pub fn spring_step(s: Spring, target: f64, dt: f64, p: Params) -> Spring {
    let dt = if dt.is_finite() {
        dt.clamp(0.0, MAX_DT_S)
    } else {
        0.0
    };
    let n = (dt / SUBSTEP_S).ceil().max(0.0) as u32;
    if n == 0 {
        return s;
    }
    let h = dt / f64::from(n);
    let mut s = s;
    for _ in 0..n {
        let accel = (-p.stiffness * (s.pos - target) - p.damping * s.vel) / p.mass;
        s.vel += accel * h;
        s.pos += s.vel * h;
    }
    s
}

/// Close enough, and slow enough, to stop.
pub fn at_rest(s: Spring, target: f64) -> bool {
    (s.pos - target).abs() < REST_DIST && s.vel.abs() < REST_SPEED
}

/// Steps and, at rest, snaps onto the target so a settled value is exact.
fn advance(s: &mut Spring, target: f64, dt: f64) {
    *s = spring_step(*s, target, dt, BOARD);
    if at_rest(*s, target) {
        *s = Spring {
            pos: target,
            vel: 0.0,
        };
    }
}

fn settled(s: Spring, target: f64) -> bool {
    s.pos == target && s.vel == 0.0
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
    to: Rect,
    alpha: Spring,
    alpha_to: f64,
    leaving: bool,
}

impl Entry {
    fn at(r: Rect, alpha: f64, alpha_to: f64) -> Self {
        let s = |v: f64| Spring { pos: v, vel: 0.0 };
        Entry {
            x: s(r.x),
            y: s(r.y),
            w: s(r.w),
            h: s(r.h),
            to: r,
            alpha: s(alpha),
            alpha_to,
            leaving: false,
        }
    }

    fn rect(&self) -> Rect {
        Rect::new(self.x.pos, self.y.pos, self.w.pos, self.h.pos)
    }

    fn settled(&self) -> bool {
        settled(self.x, self.to.x)
            && settled(self.y, self.to.y)
            && settled(self.w, self.to.w)
            && settled(self.h, self.to.h)
            && settled(self.alpha, self.alpha_to)
    }

    fn jump(&mut self, r: Rect) {
        *self = Entry::at(r, 1.0, 1.0);
    }
}

/// A spring per tile and axis. Pure: the caller says when time passes.
#[derive(Debug)]
pub struct Motion<K> {
    tiles: HashMap<K, Entry>,
}

impl<K> Default for Motion<K> {
    fn default() -> Self {
        Self {
            tiles: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Copy> Motion<K> {
    /// Where `key`'s tile should be. A tile seen for the first time stays at
    /// `to` and fades in; a known one travels from where it is now, with its
    /// speed, to `to`. Without `animate` the tile jumps, as it did before
    /// the maps moved.
    pub fn target(&mut self, key: K, to: Rect, animate: bool) {
        match self.tiles.get_mut(&key) {
            None => {
                let e = if animate {
                    Entry::at(to, 0.0, 1.0)
                } else {
                    Entry::at(to, 1.0, 1.0)
                };
                self.tiles.insert(key, e);
            }
            Some(e) if !animate => e.jump(to),
            // A tile with no area yet (the map had no size when it was made)
            // has nothing to travel from: it takes its place.
            Some(e) if e.rect().area() <= 0.0 && e.settled() => e.jump(to),
            Some(e) => {
                e.to = to;
                e.alpha_to = 1.0;
                e.leaving = false;
            }
        }
    }

    /// `key`'s tile ends: it fades out where it is, then [`Self::drain_gone`]
    /// returns it. Without `animate` it is gone at once.
    pub fn leave(&mut self, key: K, animate: bool) {
        if !animate {
            self.tiles.remove(&key);
        } else if let Some(e) = self.tiles.get_mut(&key) {
            e.alpha_to = 0.0;
            e.leaving = true;
        }
    }

    /// Time passes. Returns whether any spring is still moving.
    pub fn step(&mut self, dt: f64) -> bool {
        for e in self.tiles.values_mut() {
            advance(&mut e.x, e.to.x, dt);
            advance(&mut e.y, e.to.y, dt);
            advance(&mut e.w, e.to.w, dt);
            advance(&mut e.h, e.to.h, dt);
            advance(&mut e.alpha, e.alpha_to, dt);
        }
        self.active()
    }

    /// Some tile is not at its target: a frame callback is worth running.
    pub fn active(&self) -> bool {
        self.tiles.values().any(|e| !e.settled())
    }

    /// Where the tile is now and how opaque, `0..=1`.
    pub fn current(&self, key: K) -> Option<(Rect, f64)> {
        let e = self.tiles.get(&key)?;
        Some((e.rect(), e.alpha.pos.clamp(0.0, 1.0)))
    }

    /// Tiles that left and are now invisible; they are forgotten.
    pub fn drain_gone(&mut self) -> Vec<K> {
        let gone: Vec<K> = self
            .tiles
            .iter()
            .filter(|(_, e)| e.leaving && e.alpha.pos <= 0.0)
            .map(|(k, _)| *k)
            .collect();
        for k in &gone {
            self.tiles.remove(k);
        }
        gone
    }

    pub fn forget(&mut self, key: K) {
        self.tiles.remove(&key);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
}

/// Gives `t` the rect the spring says it is at, in whole px.
pub fn allocate(t: &impl IsA<gtk::Widget>, now: Rect) {
    let (x, y, w, h) = now.snap();
    t.measure(gtk::Orientation::Horizontal, -1);
    t.size_allocate(&gtk::Allocation::new(x, y, w.max(0), h.max(0)), -1);
}

/// Animations on, as the user set them in GTK's settings.
fn animations_enabled() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}

fn key_of(w: &impl IsA<gtk::Widget>) -> usize {
    w.upcast_ref::<gtk::Widget>().as_ptr() as usize
}

/// The motion of one map's tiles: the springs, the tiles on their way out
/// and the tick callback that runs only while something moves.
#[derive(Default)]
pub struct TileMotion {
    motion: RefCell<Motion<usize>>,
    leaving: RefCell<Vec<gtk::Widget>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_us: Cell<Option<i64>>,
}

impl TileMotion {
    /// Tell the springs where `tile` belongs now; returns the rect to
    /// allocate this frame. Sets the tile's opacity for an enter or a leave.
    /// Call from the map's `size_allocate`: it moves nothing by itself, the
    /// tick callback does.
    pub fn place(
        self: &std::rc::Rc<Self>,
        map: &gtk::Widget,
        tile: &impl IsA<gtk::Widget>,
        to: Rect,
    ) -> Rect {
        let animate = map.is_mapped() && animations_enabled();
        let key = key_of(tile);
        let (rect, alpha) = {
            let mut m = self.motion.borrow_mut();
            m.target(key, to, animate);
            m.current(key).unwrap_or((to, 1.0))
        };
        tile.set_opacity(alpha);
        self.wake(map);
        rect
    }

    /// `tile` ended. With animations it stays, fades and is unparented when
    /// invisible; without, it is unparented now.
    pub fn retire(self: &std::rc::Rc<Self>, map: &gtk::Widget, tile: &impl IsA<gtk::Widget>) {
        let animate = map.is_mapped() && animations_enabled();
        let key = key_of(tile);
        if animate && self.motion.borrow().current(key).is_some() {
            tile.set_focusable(false);
            tile.set_can_target(false);
            self.motion.borrow_mut().leave(key, true);
            self.leaving.borrow_mut().push(tile.clone().upcast());
            map.queue_allocate();
            self.wake(map);
        } else {
            self.motion.borrow_mut().forget(key);
            tile.unparent();
        }
    }

    /// Tiles on their way out with the rect to allocate each at.
    pub fn leaving(&self) -> Vec<(gtk::Widget, Rect)> {
        let m = self.motion.borrow();
        self.leaving
            .borrow()
            .iter()
            .filter_map(|t| {
                let (r, a) = m.current(key_of(t))?;
                t.set_opacity(a);
                Some((t.clone(), r))
            })
            .collect()
    }

    /// Spring still running (what the tick callback keys on).
    pub fn active(&self) -> bool {
        self.motion.borrow().active()
    }

    /// A tick callback is registered.
    pub fn ticking(&self) -> bool {
        self.tick.borrow().is_some()
    }

    /// The map goes away: unparent what is leaving and stop the callback.
    pub fn dispose(&self) {
        if let Some(id) = self.tick.take() {
            id.remove();
        }
        for t in self.leaving.take() {
            t.unparent();
        }
        self.motion.replace(Motion::default());
    }

    fn wake(self: &std::rc::Rc<Self>, map: &gtk::Widget) {
        if self.tick.borrow().is_some() || !self.motion.borrow().active() {
            return;
        }
        self.last_us.set(None);
        let me = self.clone();
        let id = map.add_tick_callback(move |map, clock| {
            let now = clock.frame_time();
            let dt = me
                .last_us
                .replace(Some(now))
                .map_or(1.0 / 60.0, |p| (now - p) as f64 / 1e6);
            let moving = me.motion.borrow_mut().step(dt);
            for key in me.motion.borrow_mut().drain_gone() {
                me.leaving.borrow_mut().retain(|t| {
                    let gone = key_of(t) == key;
                    if gone {
                        t.unparent();
                    }
                    !gone
                });
            }
            map.queue_allocate();
            if moving {
                glib::ControlFlow::Continue
            } else {
                // GTK drops the id on Break; forget it so dispose() does
                // not remove it a second time.
                me.tick.take();
                glib::ControlFlow::Break
            }
        });
        self.tick.replace(Some(id));
    }
}

/// Helpers the maps' GTK tests share.
#[cfg(test)]
pub mod testing {
    use gtk::glib;

    /// Runs `f` with `gtk-enable-animations` set to `on`, then restores it.
    pub fn animated<R>(on: bool, f: impl FnOnce() -> R) -> R {
        let s = gtk::Settings::default().expect("GTK settings");
        let was = s.is_gtk_enable_animations();
        s.set_gtk_enable_animations(on);
        let r = f();
        s.set_gtk_enable_animations(was);
        r
    }

    /// Spins the main loop until `idle` holds, at least 100 ms (the first
    /// allocation after a layout starts the springs) and at most 3 s.
    pub fn until_idle(idle: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        loop {
            let waited = start.elapsed();
            if (waited >= std::time::Duration::from_millis(100) && idle())
                || waited >= std::time::Duration::from_secs(3)
            {
                return;
            }
            pump(5, || {});
        }
    }

    /// Spins the main loop for `ms`, calling `each` after every iteration.
    pub fn pump(ms: u64, mut each: impl FnMut()) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
            each();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 1.0 / 60.0;

    fn run(s: Spring, target: f64, secs: f64) -> (Spring, f64) {
        let mut s = s;
        let mut peak = s.pos;
        for _ in 0..(secs / FPS).round() as u32 {
            s = spring_step(s, target, FPS, BOARD);
            peak = peak.max(s.pos);
        }
        (s, peak)
    }

    fn time_to_rest(from: f64, to: f64) -> f64 {
        let mut s = Spring {
            pos: from,
            vel: 0.0,
        };
        let mut t = 0.0;
        while !at_rest(s, to) && t < 5.0 {
            s = spring_step(s, to, FPS, BOARD);
            t += FPS;
        }
        t
    }

    #[test]
    fn the_board_constants_are_a_damping_ratio_of_093() {
        let zeta = BOARD.damping / (2.0 * (BOARD.stiffness * BOARD.mass).sqrt());
        assert!((zeta - 0.93).abs() < 0.01, "ratio {zeta}");
    }

    #[test]
    fn a_step_moves_toward_the_target_and_picks_up_speed() {
        let s = spring_step(Spring { pos: 0.0, vel: 0.0 }, 100.0, FPS, BOARD);
        assert!(s.pos > 0.0 && s.pos < 100.0, "pos {}", s.pos);
        assert!(s.vel > 0.0, "vel {}", s.vel);
    }

    #[test]
    fn a_spring_on_its_target_with_no_speed_stays_put() {
        let s = spring_step(
            Spring {
                pos: 40.0,
                vel: 0.0,
            },
            40.0,
            FPS,
            BOARD,
        );
        assert_eq!(
            s,
            Spring {
                pos: 40.0,
                vel: 0.0
            }
        );
    }

    #[test]
    fn the_travel_is_about_nine_tenths_done_at_a_quarter_second() {
        let (s, peak) = run(Spring::default(), 100.0, 0.25);
        assert!(s.pos > 85.0 && s.pos < 99.0, "at 0.25 s: {}", s.pos);
        assert!(peak < 100.5, "overshoot {peak}");
    }

    #[test]
    fn it_comes_to_rest_in_about_four_tenths_of_a_second() {
        for d in [20.0, 100.0, 600.0] {
            let t = time_to_rest(0.0, d);
            assert!((0.3..=0.65).contains(&t), "{d} px rested after {t} s");
        }
    }

    #[test]
    fn the_step_does_not_depend_on_the_frame_rate() {
        let (a, _) = run(Spring::default(), 100.0, 0.2);
        let mut b = Spring::default();
        for _ in 0..24 {
            b = spring_step(b, 100.0, 1.0 / 120.0, BOARD);
        }
        assert!((a.pos - b.pos).abs() < 1.0, "{} vs {}", a.pos, b.pos);
    }

    #[test]
    fn a_stalled_frame_clock_cannot_throw_the_spring() {
        let s = spring_step(Spring::default(), 100.0, 30.0, BOARD);
        assert!(s.pos.is_finite() && s.pos <= 101.0, "pos {}", s.pos);
        let s = spring_step(Spring::default(), 100.0, f64::NAN, BOARD);
        assert_eq!(s, Spring::default());
    }

    const A: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 50.0,
    };
    const B: Rect = Rect {
        x: 200.0,
        y: 80.0,
        w: 40.0,
        h: 90.0,
    };

    fn placed(at: Rect) -> Motion<u32> {
        let mut m = Motion::default();
        m.target(1, at, true);
        while m.step(FPS) {}
        m
    }

    #[test]
    fn a_new_tile_stays_at_its_rect_and_fades_in_without_a_jump() {
        let mut m = Motion::default();
        m.target(7, A, true);
        let (r, a) = m.current(7).unwrap();
        assert_eq!(r, A);
        assert_eq!(a, 0.0);
        assert!(m.active());
        m.step(FPS);
        let (r, a) = m.current(7).unwrap();
        assert_eq!(r, A);
        assert!(a > 0.0 && a < 1.0, "alpha {a}");
        while m.step(FPS) {}
        assert_eq!(m.current(7), Some((A, 1.0)));
    }

    #[test]
    fn a_moved_tile_travels_and_its_size_changes_on_the_same_spring() {
        let mut m = placed(A);
        m.target(1, B, true);
        assert!(m.active());
        assert_eq!(m.current(1).unwrap().0, A, "no jump on the first frame");
        for _ in 0..6 {
            m.step(FPS);
        }
        let r = m.current(1).unwrap().0;
        assert!(r.x > A.x && r.x < B.x, "x {}", r.x);
        assert!(r.y > A.y && r.y < B.y, "y {}", r.y);
        assert!(r.w < A.w && r.w > B.w, "w {}", r.w);
        assert!(r.h > A.h && r.h < B.h, "h {}", r.h);
    }

    #[test]
    fn a_moving_tile_settles_exactly_on_its_target_and_the_ticks_stop() {
        let mut m = placed(A);
        m.target(1, B, true);
        let mut frames = 0;
        while m.step(FPS) {
            frames += 1;
            assert!(frames < 120, "never settled");
        }
        assert!(frames >= 10, "arrived in {frames} frames");
        assert_eq!(m.current(1).unwrap().0, B);
        assert!(!m.active());
        // Settled: stepping again changes nothing and stays quiet.
        assert!(!m.step(FPS));
        assert_eq!(m.current(1).unwrap().0, B);
    }

    #[test]
    fn a_tile_with_no_area_takes_its_place_instead_of_growing_from_nothing() {
        let mut m = placed(Rect::new(0.0, 0.0, 0.0, 0.0));
        m.target(1, B, true);
        assert_eq!(m.current(1), Some((B, 1.0)));
        assert!(!m.active());
    }

    #[test]
    fn the_same_target_again_starts_nothing() {
        let mut m = placed(A);
        m.target(1, A, true);
        assert!(!m.active());
    }

    #[test]
    fn a_retarget_in_flight_keeps_the_position_and_the_speed() {
        let mut m = placed(A);
        m.target(1, B, true);
        for _ in 0..5 {
            m.step(FPS);
        }
        let before = m.current(1).unwrap().0;
        m.target(1, A, true);
        assert_eq!(m.current(1).unwrap().0, before, "no jump on retarget");
        m.step(FPS);
        // Still carried toward B by its speed for a moment.
        assert!(m.current(1).unwrap().0.x >= before.x - 1.0);
        while m.step(FPS) {}
        assert_eq!(m.current(1).unwrap().0, A);
    }

    #[test]
    fn with_animations_off_a_tile_jumps_and_nothing_ticks() {
        let mut m = placed(A);
        m.target(1, B, false);
        assert_eq!(m.current(1), Some((B, 1.0)));
        assert!(!m.active());
        m.target(2, A, false);
        assert_eq!(m.current(2), Some((A, 1.0)));
        assert!(!m.active());
    }

    #[test]
    fn a_tile_that_ends_fades_where_it_is_and_is_forgotten_when_invisible() {
        let mut m = placed(A);
        m.leave(1, true);
        assert_eq!(m.current(1), Some((A, 1.0)));
        assert!(m.drain_gone().is_empty());
        m.step(FPS);
        let (r, a) = m.current(1).unwrap();
        assert_eq!(r, A);
        assert!(a < 1.0, "alpha {a}");
        while m.step(FPS) {}
        assert_eq!(m.drain_gone(), vec![1]);
        assert_eq!(m.len(), 0);
        assert!(!m.active());
    }

    #[test]
    fn a_tile_that_ends_without_animations_is_gone_at_once() {
        let mut m = placed(A);
        m.leave(1, false);
        assert_eq!(m.current(1), None);
    }

    #[test]
    fn a_tile_that_comes_back_while_fading_out_stays() {
        let mut m = placed(A);
        m.leave(1, true);
        m.step(FPS);
        m.target(1, A, true);
        while m.step(FPS) {}
        assert!(m.drain_gone().is_empty());
        assert_eq!(m.current(1), Some((A, 1.0)));
    }
}
