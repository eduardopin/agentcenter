//! Memory map layout (direction B): pure geometry, no GTK.
//!
//! - [`squarify`]: the squarified treemap of Bruls, Huizing and van Wijk
//!   (2000). Rows of tiles are laid along the short side of what is left,
//!   and a tile joins the row while the row's worst aspect ratio improves.
//! - [`layout`]: the nested map. Groups in the fixed order of
//!   [`TileState::ORDER`], each a block of its sessions sorted by weight
//!   (PSS + swap). Tiles
//!   under [`MERGE_BELOW_PX2`] merge into one "N small" tile per state; a
//!   waiting session is never merged.
//! - [`label_fit`]: what a tile can show, from widths the caller measured
//!   (Pango in the widget, a fake in the tests).
//! - [`DriftStability`]: lay out again only when a tile's share drifts more
//!   than 20 % or a session starts or ends, at most every 30 s; otherwise the
//!   widget keeps its rects and only updates the numbers. Both maps use it.
//! - [`nearest`]: the tile an arrow key moves to.

use std::collections::BTreeMap;

/// Rectangle in px, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn area(&self) -> f64 {
        self.w * self.h
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Area shared with `o` (0 when they only touch).
    pub fn overlap(&self, o: &Rect) -> f64 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    /// `o` lies inside, with `eps` of slack for float error.
    pub fn contains(&self, o: &Rect, eps: f64) -> bool {
        o.x >= self.x - eps
            && o.y >= self.y - eps
            && o.right() <= self.right() + eps
            && o.bottom() <= self.bottom() + eps
    }

    /// Edges rounded to whole px: `(x, y, w, h)`. Neighbours share an edge
    /// before rounding, so they still share it after.
    pub fn snap(&self) -> (i32, i32, i32, i32) {
        let (x0, y0) = (self.x.round() as i32, self.y.round() as i32);
        let (x1, y1) = (self.right().round() as i32, self.bottom().round() as i32);
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// Squarified treemap of `weights` in `area`, heaviest first. Returns one
/// rect per weight, in the order of `weights`. Weights that are not finite
/// and positive get an empty rect at the area's origin.
pub fn squarify(weights: &[f64], area: Rect) -> Vec<Rect> {
    let mut order = usable(weights);
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    lay_rows(&order, weights.len(), area)
}

/// [`squarify`] without the sort: rows follow the given order, so the first
/// weight sits at the top-left and every later one to the right of or below
/// the earlier ones. The map uses it for the groups, whose order is fixed.
pub fn squarify_ordered(weights: &[f64], area: Rect) -> Vec<Rect> {
    lay_rows(&usable(weights), weights.len(), area)
}

/// `(index, weight)` of the finite, positive weights.
fn usable(weights: &[f64]) -> Vec<(usize, f64)> {
    weights
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, w)| w.is_finite() && *w > 0.0)
        .collect()
}

/// Worst aspect ratio of a row of `areas` laid along a side of `side` px.
fn worst(areas: &[f64], side: f64) -> f64 {
    let sum: f64 = areas.iter().sum();
    let (min, max) = areas.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), a| {
        (lo.min(*a), hi.max(*a))
    });
    let (s2, sum2) = (side * side, sum * sum);
    (s2 * max / sum2).max(sum2 / (s2 * min))
}

/// The squarify loop over `order` (already in placing order): a tile joins
/// the current row while that does not worsen the row's worst ratio.
fn lay_rows(order: &[(usize, f64)], n: usize, area: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::new(area.x, area.y, 0.0, 0.0); n];
    let total: f64 = order.iter().map(|o| o.1).sum();
    if total <= 0.0 || area.area() <= 0.0 {
        return out;
    }
    let k = area.area() / total;
    let mut rest = area;
    let mut row: Vec<(usize, f64)> = Vec::new();
    let mut areas: Vec<f64> = Vec::new();
    for (i, w) in order {
        let a = w * k;
        let side = rest.w.min(rest.h);
        if !areas.is_empty() {
            areas.push(a);
            let with = worst(&areas, side);
            areas.pop();
            if with > worst(&areas, side) {
                rest = place(&row, rest, false, &mut out);
                row.clear();
                areas.clear();
            }
        }
        row.push((*i, a));
        areas.push(a);
    }
    place(&row, rest, true, &mut out);
    out
}

/// Lays `row` along the short side of `rest` and returns what is left. The
/// last row takes all of `rest`, and the last tile of a row runs to the
/// row's end, so float error never leaves a sliver.
fn place(row: &[(usize, f64)], rest: Rect, last: bool, out: &mut [Rect]) -> Rect {
    let sum: f64 = row.iter().map(|r| r.1).sum();
    if rest.w >= rest.h {
        let cw = if last { rest.w } else { sum / rest.h };
        let mut y = rest.y;
        for (n, (i, a)) in row.iter().enumerate() {
            let h = if n + 1 == row.len() {
                rest.bottom() - y
            } else {
                a / cw
            };
            out[*i] = Rect::new(rest.x, y, cw, h);
            y += h;
        }
        Rect::new(rest.x + cw, rest.y, rest.w - cw, rest.h)
    } else {
        let rh = if last { rest.h } else { sum / rest.w };
        let mut x = rest.x;
        for (n, (i, a)) in row.iter().enumerate() {
            let w = if n + 1 == row.len() {
                rest.right() - x
            } else {
                a / rh
            };
            out[*i] = Rect::new(x, rest.y, w, rh);
            x += w;
        }
        Rect::new(rest.x, rest.y + rh, rest.w, rest.h - rh)
    }
}

/// Session state as the map groups it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TileState {
    Waiting,
    Busy,
    Idle,
    Multiplexer,
}

impl TileState {
    /// Group order on the map: what needs you first.
    pub const ORDER: [TileState; 4] = [
        TileState::Waiting,
        TileState::Busy,
        TileState::Idle,
        TileState::Multiplexer,
    ];

    /// Word for labels and CSS classes.
    pub fn word(self) -> &'static str {
        match self {
            TileState::Waiting => "waiting",
            TileState::Busy => "busy",
            TileState::Idle => "idle",
            TileState::Multiplexer => "multiplexer",
        }
    }
}

/// One session to lay out: its pid, state and weight (PSS + swap, bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub id: u32,
    pub state: TileState,
    pub weight: f64,
}

/// Below this area (px²) a tile cannot carry a readable label.
pub const MERGE_BELOW_PX2: f64 = 4200.0;

/// What a tile stands for: one session, or the merged small ones of a state.
#[derive(Debug, Clone, PartialEq)]
pub enum TileKind {
    Session(u32),
    Merged(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    pub state: TileState,
    pub kind: TileKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    /// Group blocks, in [`TileState::ORDER`], empty groups left out.
    pub groups: Vec<(TileState, Rect)>,
    pub tiles: Vec<Tile>,
}

/// The nested map of `items` in `bounds`. `merge` turns the small-tile merge on (off on the Reclaim page,
/// where every idle session is a target). Items without a positive weight
/// are left out.
pub fn layout(items: &[Item], bounds: Rect, merge: bool) -> Layout {
    layout_inner(items, bounds, merge, &|_| false)
}

/// [`layout`] with the `pinned` ids kept out of every merge.
fn layout_inner(items: &[Item], bounds: Rect, merge: bool, pinned: &dyn Fn(u32) -> bool) -> Layout {
    let items: Vec<&Item> = items
        .iter()
        .filter(|i| i.weight.is_finite() && i.weight > 0.0)
        .collect();
    if items.is_empty() {
        return Layout::default();
    }
    let map = bounds;

    let groups: Vec<(TileState, Vec<&Item>)> = TileState::ORDER
        .iter()
        .map(|st| {
            (
                *st,
                items.iter().copied().filter(|i| i.state == *st).collect(),
            )
        })
        .filter(|(_, v): &(TileState, Vec<&Item>)| !v.is_empty())
        .collect();
    let weights: Vec<f64> = groups
        .iter()
        .map(|(_, v)| v.iter().map(|i| i.weight).sum())
        .collect();
    let rects = squarify_ordered(&weights, map);

    let mut out = Layout::default();
    for (((state, members), rect), total) in groups.into_iter().zip(rects).zip(weights) {
        out.groups.push((state, rect));
        let px_per_unit = rect.area() / total;
        let small = |i: &&Item| {
            merge
                && i.state != TileState::Waiting
                && !pinned(i.id)
                && i.weight * px_per_unit < MERGE_BELOW_PX2
        };
        let n_small = members.iter().filter(|i| small(i)).count();
        let mut kinds: Vec<TileKind> = Vec::new();
        let mut tile_weights: Vec<f64> = Vec::new();
        let mut merged: Vec<(u32, f64)> = Vec::new();
        for i in &members {
            if n_small >= 2 && small(i) {
                merged.push((i.id, i.weight));
            } else {
                kinds.push(TileKind::Session(i.id));
                tile_weights.push(i.weight);
            }
        }
        if !merged.is_empty() {
            merged.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            tile_weights.push(merged.iter().map(|m| m.1).sum());
            kinds.push(TileKind::Merged(merged.into_iter().map(|m| m.0).collect()));
        }
        for (kind, rect) in kinds.into_iter().zip(squarify(&tile_weights, rect)) {
            out.tiles.push(Tile { rect, state, kind });
        }
    }
    out
}

/// Floor of a pinned tile, px (the token map's b2+ tiles).
pub const PINNED_MIN_W_PX: f64 = 64.0;
pub const PINNED_MIN_H_PX: f64 = 40.0;
/// Rounds of growing under-floor pinned tiles before giving up (the bounds
/// may simply be too small for every pin).
const PIN_ROUNDS: usize = 12;

/// [`layout`] where the `pinned` ids are never merged and
/// get at least [`PINNED_MIN_W_PX`] x [`PINNED_MIN_H_PX`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn layout_pinned(items: &[Item], bounds: Rect, pinned: &dyn Fn(u32) -> bool) -> Layout {
    layout_pinned_floor(items, bounds, pinned, (PINNED_MIN_W_PX, PINNED_MIN_H_PX))
}

/// [`layout_pinned`] with a floor of `(w, h)` px. A pinned tile under the
/// floor has its weight raised until it clears it, so its area then exceeds
/// its true share and the others give way; the caller keeps the true
/// weights for the numbers.
pub fn layout_pinned_floor(
    items: &[Item],
    bounds: Rect,
    pinned: &dyn Fn(u32) -> bool,
    floor: (f64, f64),
) -> Layout {
    let mut grown: Vec<Item> = items.to_vec();
    let mut out = layout_inner(&grown, bounds, true, pinned);
    for _ in 0..PIN_ROUNDS {
        let mut changed = false;
        for t in &out.tiles {
            let TileKind::Session(id) = t.kind else {
                continue;
            };
            if !pinned(id) || (t.rect.w >= floor.0 - 0.01 && t.rect.h >= floor.1 - 0.01) {
                continue;
            }
            let f = (floor.0 / t.rect.w.max(1.0)).max(floor.1 / t.rect.h.max(1.0));
            if let Some(i) = grown.iter_mut().find(|i| i.id == id) {
                i.weight *= (f * f * 1.02).min(100.0);
                changed = true;
            }
        }
        if !changed {
            break;
        }
        out = layout_inner(&grown, bounds, true, pinned);
    }
    out
}

/// What a tile shows, by size and measured text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelFit {
    /// Too small for anything.
    Nothing,
    /// The state glyph alone.
    Glyph,
    /// Glyph, then the value below.
    GlyphValue,
    /// One row for a wide, short tile: glyph, the whole name, then the value
    /// on the same line.
    Inline,
    /// Glyph and name on one line (ellipsized when longer than the line),
    /// value below.
    Name,
    /// Name wrapped once: the first line is `name[..split]`, ending at a
    /// hyphen; the rest goes to a second line (and ellipsizes there).
    Wrapped(usize),
    /// A narrow tile: glyph row, then the name on its own row across the
    /// whole width (ellipsized), then the value when `value` says it fits.
    Stacked { value: bool },
}

/// Tiles at least this wide and tall get the glyph.
pub const GLYPH_MIN_PX: f64 = 22.0;
/// Inner padding of a tile, px.
pub const TILE_PAD_PX: f64 = 8.0;
/// Glyph size, px.
pub const GLYPH_PX: f64 = 11.0;
/// Room the first line loses to padding, glyph and gap.
pub const LINE1_INSET_PX: f64 = 32.0;
/// Room the second line (and the value) loses to padding.
pub const LINE2_INSET_PX: f64 = 16.0;
/// Height for glyph row plus value row.
pub const VALUE_MIN_H_PX: f64 = 40.0;
/// Height for the one-row label of a wide, short tile.
pub const INLINE_MIN_H_PX: f64 = 26.0;
/// Gap between the name and the value on that row.
pub const INLINE_GAP_PX: f64 = 8.0;
/// Height for two name rows plus the value row.
pub const WRAP_MIN_H_PX: f64 = 58.0;

/// Byte offsets after each place a name may break: after a hyphen, or after
/// a run of whitespace.
fn break_points(name: &str) -> impl Iterator<Item = usize> + '_ {
    name.char_indices()
        .filter(|&(_, c)| c == '-' || c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
}

/// The label rule. `value_w`: measured width of the value text; `measure`:
/// width of a piece of the name in the name font. A name shows only when its
/// first word fits beside the glyph; it wraps once at a hyphen or a space
/// when it has the height, and otherwise ellipsizes.
pub fn label_fit(
    name: &str,
    w: f64,
    h: f64,
    value_w: f64,
    measure: impl Fn(&str) -> f64,
) -> LabelFit {
    if w < GLYPH_MIN_PX || h < GLYPH_MIN_PX {
        return LabelFit::Nothing;
    }
    if h < VALUE_MIN_H_PX {
        let row = measure(name) + INLINE_GAP_PX + value_w;
        return if h >= INLINE_MIN_H_PX && row <= w - LINE1_INSET_PX {
            LabelFit::Inline
        } else {
            LabelFit::Glyph
        };
    }
    // A narrow tile that cannot carry the name beside the glyph puts it on
    // its own row, across the whole width, from NAME_MIN_W_PX up.
    let stacked = || {
        let value = h >= WRAP_MIN_H_PX && value_w <= w - LINE2_INSET_PX;
        LabelFit::Stacked { value }
    };
    if value_w > w - LINE2_INSET_PX {
        return if w >= NAME_MIN_W_PX {
            stacked()
        } else {
            LabelFit::Glyph
        };
    }
    let line1 = w - LINE1_INSET_PX;
    if measure(name) <= line1 {
        return LabelFit::Name;
    }
    if h >= WRAP_MIN_H_PX {
        let split = break_points(name)
            .filter(|&i| i < name.len() && measure(name[..i].trim_end()) <= line1)
            .last();
        if let Some(split) = split {
            return LabelFit::Wrapped(split);
        }
    }
    let first = break_points(name)
        .next()
        .map_or(name, |i| name[..i].trim_end());
    if measure(first) <= line1 {
        LabelFit::Name
    } else {
        if w >= NAME_MIN_W_PX {
            stacked()
        } else {
            LabelFit::GlyphValue
        }
    }
}

/// Relayout at most this often, ms.
pub const RELAYOUT_MIN_MS: u64 = 30_000;
/// A tile whose share moved this much (a fraction) wants a new layout.
pub const DRIFT: f64 = 0.20;
/// The name is drawn from this tile width up, px.
pub const NAME_MIN_W_PX: f64 = 36.0;
/// Tiles at least this wide get the larger name font, px.
pub const NAME_BIG_FROM_W_PX: f64 = 96.0;

/// Name font size by tile width, px: 12 bold below 96 px tiles, 13 above.
pub fn name_px(w: f64) -> f64 {
    if w >= NAME_BIG_FROM_W_PX { 13.0 } else { 12.0 }
}

/// When to lay out again: a tile's share of the total drifted more than
/// [`DRIFT`] from the share it was laid out with, or a session started or
/// ended, and [`RELAYOUT_MIN_MS`] passed since the last layout. Nothing
/// else, no timer of its own. Holding the layout under the pointer or the
/// focus is the widget's business.
#[derive(Debug, Default)]
pub struct DriftStability {
    shares: BTreeMap<u32, f64>,
    at_ms: Option<u64>,
}

fn shares_of(weights: &[(u32, f64)]) -> BTreeMap<u32, f64> {
    let usable = |w: f64| w.is_finite() && w > 0.0;
    let total: f64 = weights.iter().map(|w| w.1).filter(|w| usable(*w)).sum();
    weights
        .iter()
        .filter(|w| usable(w.1))
        .map(|&(id, w)| (id, w / total))
        .collect()
}

impl DriftStability {
    /// Forget the last layout: the next call lays out (size change).
    pub fn invalidate(&mut self) {
        self.at_ms = None;
    }

    /// Records a layout made for `weights` at `now_ms`.
    pub fn rebase(&mut self, weights: &[(u32, f64)], now_ms: u64) {
        self.shares = shares_of(weights);
        self.at_ms = Some(now_ms);
    }

    /// `true` when the map must lay out again; records the layout when it
    /// says yes. A clock that went back (a fixture restarting) says yes.
    pub fn should_relayout(&mut self, weights: &[(u32, f64)], now_ms: u64) -> bool {
        let due = match self.at_ms {
            None => true,
            Some(at) if now_ms < at => true,
            Some(at) if now_ms - at < RELAYOUT_MIN_MS => false,
            Some(_) => {
                let now = shares_of(weights);
                now.keys().ne(self.shares.keys())
                    || now
                        .iter()
                        .any(|(id, s)| (s / self.shares[id] - 1.0).abs() > DRIFT)
            }
        };
        if due {
            self.rebase(weights, now_ms);
        }
        due
    }
}

/// Arrow direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The tile an arrow key moves to from `rects[from]`: among the tiles past
/// its edge in `dir`, those that overlap it across the direction first, then
/// the smallest gap, then the closest centre. `None` at the map's edge.
pub fn nearest(rects: &[Rect], from: usize, dir: Dir) -> Option<usize> {
    const SLACK: f64 = 1e-6;
    let f = rects.get(from)?;
    let span = |a0: f64, a1: f64, b0: f64, b1: f64| a1.min(b1) - a0.max(b0);
    let (fx, fy) = f.center();
    let mut best: Option<(usize, bool, f64, f64)> = None;
    for (i, r) in rects.iter().enumerate() {
        if i == from || r.area() <= 0.0 {
            continue;
        }
        let (gap, across) = match dir {
            Dir::Right => (r.x - f.right(), span(f.y, f.bottom(), r.y, r.bottom())),
            Dir::Left => (f.x - r.right(), span(f.y, f.bottom(), r.y, r.bottom())),
            Dir::Down => (r.y - f.bottom(), span(f.x, f.right(), r.x, r.right())),
            Dir::Up => (f.y - r.bottom(), span(f.x, f.right(), r.x, r.right())),
        };
        if gap < -SLACK {
            continue;
        }
        let apart = across <= SLACK;
        // Apart from the source across the direction, only the centre counts.
        let gap = if apart { 0.0 } else { gap.max(0.0) };
        let (rx, ry) = r.center();
        let dist = (rx - fx).hypot(ry - fy);
        let better = match best {
            None => true,
            Some((_, b_apart, b_gap, b_dist)) => (apart, gap, dist) < (b_apart, b_gap, b_dist),
        };
        if better {
            best = Some((i, apart, gap, dist));
        }
    }
    best.map(|b| b.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    fn no_overlaps(rects: &[Rect]) {
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(a.overlap(b) < EPS, "{a:?} overlaps {b:?}");
            }
        }
    }

    fn item(id: u32, state: TileState, weight: f64) -> Item {
        Item { id, state, weight }
    }

    /// Weights of the tipico-40 fixture in GiB × 100 (pss + swap), by state.
    fn tipico() -> Vec<Item> {
        use TileState::*;
        let w: [(TileState, f64); 40] = [
            (Busy, 109.0),
            (Idle, 108.0),
            (Idle, 96.0),
            (Busy, 94.0),
            (Idle, 93.0),
            (Idle, 91.0),
            (Idle, 90.0),
            (Multiplexer, 89.0),
            (Busy, 88.0),
            (Idle, 87.0),
            (Idle, 86.0),
            (Busy, 85.0),
            (Idle, 85.0),
            (Idle, 85.0),
            (Idle, 83.0),
            (Multiplexer, 82.0),
            (Idle, 81.0),
            (Idle, 80.0),
            (Idle, 80.0),
            (Busy, 77.0),
            (Idle, 71.0),
            (Busy, 71.0),
            (Idle, 67.0),
            (Idle, 65.0),
            (Idle, 64.0),
            (Idle, 63.0),
            (Busy, 62.0),
            (Idle, 59.0),
            (Idle, 58.0),
            (Idle, 58.0),
            (Idle, 54.0),
            (Idle, 53.0),
            (Waiting, 53.0),
            (Idle, 48.0),
            (Idle, 44.0),
            (Busy, 40.0),
            (Waiting, 38.0),
            (Busy, 36.0),
            (Waiting, 27.0),
            (Idle, 20.0),
        ];
        w.iter()
            .enumerate()
            .map(|(i, (s, v))| item(i as u32, *s, *v))
            .collect()
    }

    const MAP: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 760.0,
        h: 466.0,
    };

    #[test]
    fn squarify_areas_are_proportional_inside_and_disjoint() {
        let weights = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let area = Rect::new(10.0, 20.0, 600.0, 400.0);
        let rects = squarify(&weights, area);
        assert_eq!(rects.len(), weights.len());
        let total: f64 = weights.iter().sum();
        for (r, w) in rects.iter().zip(weights) {
            assert!(
                (r.area() - area.area() * w / total).abs() < 1e-6,
                "{r:?} for {w}"
            );
            assert!(area.contains(r, EPS), "{r:?} outside");
        }
        no_overlaps(&rects);
        let covered: f64 = rects.iter().map(Rect::area).sum();
        assert!((covered - area.area()).abs() < 1e-6, "covers the area");
    }

    /// The paper's example (6,6,4,3,2,2,1 in 6×4): the first row is the two
    /// 6s side by side along the short side, aspect ratio 1.5 at worst.
    #[test]
    fn squarify_keeps_tiles_square_on_the_paper_example() {
        let rects = squarify(
            &[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0],
            Rect::new(0.0, 0.0, 6.0, 4.0),
        );
        assert!(
            (rects[0].w - 3.0).abs() < 1e-9 && (rects[0].h - 2.0).abs() < 1e-9,
            "{:?}",
            rects[0]
        );
        let worst = rects
            .iter()
            .map(|r| (r.w / r.h).max(r.h / r.w))
            .fold(0.0, f64::max);
        assert!(worst <= 3.0, "worst aspect {worst}");
    }

    #[test]
    fn squarify_gives_unusable_weights_an_empty_rect_and_keeps_order() {
        let rects = squarify(
            &[0.0, 3.0, f64::NAN, 1.0, -2.0],
            Rect::new(0.0, 0.0, 40.0, 10.0),
        );
        assert_eq!(rects.len(), 5);
        assert_eq!(rects[0].area(), 0.0);
        assert_eq!(rects[2].area(), 0.0);
        assert_eq!(rects[4].area(), 0.0);
        assert!((rects[1].area() - 300.0).abs() < 1e-9);
        assert!((rects[3].area() - 100.0).abs() < 1e-9);
        assert!(squarify(&[], MAP).is_empty());
    }

    #[test]
    fn squarify_ordered_places_later_weights_right_of_or_below() {
        let weights = [1.0, 9.0, 3.0, 5.0];
        let rects = squarify_ordered(&weights, MAP);
        assert!(
            rects[0].x.abs() < EPS && rects[0].y.abs() < EPS,
            "first at top-left"
        );
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let (a, b) = (rects[i], rects[j]);
                assert!(
                    b.x >= a.right() - EPS || b.y >= a.bottom() - EPS,
                    "{j} {b:?} vs {i} {a:?}"
                );
            }
        }
        no_overlaps(&rects);
    }

    #[test]
    fn layout_tiles_are_proportional_inside_bounds_and_disjoint() {
        let items = tipico();
        let bounds = Rect::new(4.0, 2.0, 760.0, 466.0);
        let l = layout(&items, bounds, false);
        assert_eq!(l.tiles.len(), 40, "no merge asked");
        let total: f64 = items.iter().map(|i| i.weight).sum::<f64>();
        let usable = bounds.w * bounds.h;
        for t in &l.tiles {
            let TileKind::Session(id) = t.kind else {
                panic!("merged without merge")
            };
            let w = items[id as usize].weight;
            assert!((t.rect.area() - usable * w / total).abs() < 1e-6, "{t:?}");
            assert!(bounds.contains(&t.rect, EPS));
            assert_eq!(t.state, items[id as usize].state);
        }
        let all: Vec<Rect> = l.tiles.iter().map(|t| t.rect).collect();
        no_overlaps(&all);
        // Snapped to whole px, neighbours still never overlap.
        let snapped: Vec<Rect> = all
            .iter()
            .map(|r| {
                let (x, y, w, h) = r.snap();
                Rect::new(x.into(), y.into(), w.into(), h.into())
            })
            .collect();
        no_overlaps(&snapped);
    }

    #[test]
    fn layout_groups_follow_the_fixed_order_and_hold_their_tiles() {
        let l = layout(&tipico(), MAP, true);
        let states: Vec<TileState> = l.groups.iter().map(|g| g.0).collect();
        assert_eq!(states, TileState::ORDER.to_vec());
        let first = l.groups[0].1;
        assert!(
            first.x.abs() < EPS && first.y.abs() < EPS,
            "waiting at top-left"
        );
        for i in 0..l.groups.len() {
            for j in i + 1..l.groups.len() {
                let (a, b) = (l.groups[i].1, l.groups[j].1);
                assert!(b.x >= a.right() - EPS || b.y >= a.bottom() - EPS);
            }
        }
        for t in &l.tiles {
            let g = l.groups.iter().find(|g| g.0 == t.state).expect("group");
            assert!(g.1.contains(&t.rect, EPS), "{t:?} outside its group");
        }
        // Empty groups are left out, the order of the rest stays.
        let only = [
            item(1, TileState::Multiplexer, 2.0),
            item(2, TileState::Busy, 1.0),
        ];
        let l = layout(&only, MAP, true);
        let states: Vec<TileState> = l.groups.iter().map(|g| g.0).collect();
        assert_eq!(states, [TileState::Busy, TileState::Multiplexer]);
    }

    #[test]
    fn small_tiles_merge_per_state_and_waiting_never_merges() {
        use TileState::*;
        // In 400×300: 1 unit = 120 px². Weights 20 = 2400 px²
        // are small; 100 = 12000 px² are not.
        let mut items = vec![item(1, Idle, 300.0), item(2, Busy, 300.0)];
        items.extend([
            item(10, Idle, 20.0),
            item(11, Idle, 20.0),
            item(12, Idle, 20.0),
        ]);
        items.extend([item(20, Busy, 20.0), item(21, Busy, 20.0)]);
        items.extend([item(30, Waiting, 20.0), item(31, Waiting, 20.0)]);
        items.push(item(40, Multiplexer, 20.0));
        let total: f64 = items.iter().map(|i| i.weight).sum();
        let bounds = Rect::new(0.0, 0.0, 400.0, total * 120.0 / 400.0);
        let l = layout(&items, bounds, true);
        let merged: Vec<(TileState, Vec<u32>)> = l
            .tiles
            .iter()
            .filter_map(|t| match &t.kind {
                TileKind::Merged(ids) => {
                    let mut ids = ids.clone();
                    ids.sort();
                    Some((t.state, ids))
                }
                TileKind::Session(_) => None,
            })
            .collect();
        assert!(merged.contains(&(Idle, vec![10, 11, 12])), "{merged:?}");
        assert!(merged.contains(&(Busy, vec![20, 21])), "{merged:?}");
        assert_eq!(
            merged.len(),
            2,
            "waiting never merges; one small alone stays: {merged:?}"
        );
        for id in [30, 31, 40, 1, 2] {
            assert!(
                l.tiles.iter().any(|t| t.kind == TileKind::Session(id)),
                "{id} kept"
            );
        }
        // The merged tile has the area of its sessions together.
        let m = l
            .tiles
            .iter()
            .find(|t| matches!(&t.kind, TileKind::Merged(v) if v.len() == 3));
        assert!((m.expect("idle merged").rect.area() - 3.0 * 2400.0).abs() < 1e-6);
        // Without merge every session keeps its tile.
        assert_eq!(layout(&items, bounds, false).tiles.len(), items.len());
    }

    /// The map has no free RAM column (free RAM is the machine strip's RAM bar): the
    /// session groups cover the whole bounds.
    #[test]
    fn sessions_fill_the_whole_bounds_with_no_free_column() {
        let items = [
            item(1, TileState::Busy, 300.0),
            item(2, TileState::Idle, 100.0),
        ];
        let l = layout(&items, MAP, true);
        let area: f64 = l.groups.iter().map(|g| g.1.area()).sum();
        assert!((area - MAP.area()).abs() < 1e-6, "{area} of {}", MAP.area());
        let right = l.tiles.iter().map(|t| t.rect.right()).fold(0.0, f64::max);
        assert!((right - MAP.right()).abs() < EPS, "reaches the right edge");
    }

    #[test]
    fn empty_input_and_one_session() {
        let l = layout(&[], MAP, true);
        assert!(l.tiles.is_empty() && l.groups.is_empty());
        let l = layout(&[item(7, TileState::Idle, 0.0)], MAP, true);
        assert!(l.tiles.is_empty(), "no weight, no tile");
        let l = layout(&[item(7, TileState::Waiting, 3.0)], MAP, true);
        assert_eq!(l.tiles.len(), 1);
        assert_eq!(l.tiles[0].rect, MAP);
        assert_eq!(l.tiles[0].kind, TileKind::Session(7));
        assert_eq!(l.groups, vec![(TileState::Waiting, MAP)]);
    }

    /// 7 px per character: a stand-in for Pango in the tests.
    fn seven(s: &str) -> f64 {
        7.0 * s.chars().count() as f64
    }

    #[test]
    fn label_rule_cases() {
        // "kiln-cache-layer" is 112 px; "kiln-" 35; "kiln-cache-" 77.
        let n = "kiln-cache-layer";
        assert_eq!(label_fit(n, 21.0, 80.0, 56.0, seven), LabelFit::Nothing);
        assert_eq!(label_fit(n, 80.0, 21.0, 56.0, seven), LabelFit::Nothing);
        assert_eq!(
            label_fit(n, 22.0, 22.0, 56.0, seven),
            LabelFit::Glyph,
            "22 px gets the glyph"
        );
        assert_eq!(
            label_fit(n, 60.0, 39.0, 56.0, seven),
            LabelFit::Glyph,
            "no room for the value row"
        );
        assert_eq!(
            label_fit(n, 60.0, 60.0, 40.0, seven),
            LabelFit::Stacked { value: true },
            "kiln- (35) does not fit in 28: the name goes on its own row"
        );
        assert_eq!(
            label_fit(n, 70.0, 60.0, 40.0, seven),
            LabelFit::Wrapped(5),
            "kiln- (35) fits in 38"
        );
        assert_eq!(
            label_fit(n, 144.0, 40.0, 56.0, seven),
            LabelFit::Name,
            "112 fits in 112"
        );
        assert_eq!(
            label_fit(n, 110.0, 60.0, 56.0, seven),
            LabelFit::Wrapped(11),
            "kiln-cache- fits in 78"
        );
        assert_eq!(
            label_fit(n, 100.0, 60.0, 56.0, seven),
            LabelFit::Wrapped(5),
            "only kiln- fits in 68"
        );
        assert_eq!(
            label_fit(n, 110.0, 50.0, 56.0, seven),
            LabelFit::Name,
            "no height to wrap: ellipsize"
        );
        assert_eq!(
            label_fit("ledgerauthrefactor", 100.0, 60.0, 56.0, seven),
            LabelFit::Stacked { value: true },
            "no hyphen, first word too long: its own row"
        );
        assert_eq!(label_fit("orbit", 72.0, 60.0, 56.0, seven), LabelFit::Name);
        assert_eq!(
            label_fit(n, 60.0, 60.0, 56.0, seven),
            LabelFit::Stacked { value: false },
            "value 56 needs 72: the name still shows"
        );
    }

    #[test]
    fn a_wide_short_tile_labels_itself_on_one_row() {
        // "herdr multiplexer" is 119 px, the value 56, the gap 8: 183 of the
        // 192 px the row has.
        let n = "herdr multiplexer";
        assert_eq!(label_fit(n, 224.0, 37.0, 56.0, seven), LabelFit::Inline);
        assert_eq!(label_fit(n, 224.0, 26.0, 56.0, seven), LabelFit::Inline);
        assert_eq!(
            label_fit(n, 224.0, 25.0, 56.0, seven),
            LabelFit::Glyph,
            "one row needs 26 px"
        );
        assert_eq!(
            label_fit(n, 190.0, 37.0, 56.0, seven),
            LabelFit::Glyph,
            "name and value must both fit"
        );
        assert_eq!(
            label_fit(n, 224.0, 40.0, 56.0, seven),
            LabelFit::Name,
            "40 px keeps the two-row layout"
        );
    }

    #[test]
    fn label_breaks_at_whitespace_as_well_as_hyphens() {
        // "ledger cache layer": "ledger " is 49 px, "ledger cache " 91.
        let n = "ledger cache layer";
        assert_eq!(
            label_fit(n, 90.0, 60.0, 56.0, seven),
            LabelFit::Wrapped(7),
            "ledger fits in 58, ledger cache (91) does not"
        );
        assert_eq!(
            label_fit(n, 126.0, 60.0, 56.0, seven),
            LabelFit::Wrapped(13),
            "ledger cache fits in 94"
        );
        assert_eq!(
            label_fit("relay-i18n pass now", 110.0, 60.0, 56.0, seven),
            LabelFit::Wrapped(11),
            "a hyphen and a space both break: the later fitting one wins"
        );
        assert_eq!(
            label_fit(n, 60.0, 60.0, 40.0, seven),
            LabelFit::Stacked { value: true },
            "the first word does not fit in 28: its own row"
        );
    }

    #[test]
    fn pinned_tiles_are_never_merged_and_keep_the_floor() {
        let bounds = Rect::new(0.0, 0.0, 400.0, 240.0);
        let mut items = vec![item(1, TileState::Idle, 1000.0)];
        for id in 10..20 {
            items.push(item(id, TileState::Idle, 5.0));
        }
        for id in [30, 31, 32] {
            items.push(item(id, TileState::Idle, 5.0));
        }
        let pinned = |id: u32| id >= 30;
        let l = layout_pinned(&items, bounds, &pinned);
        for id in [30, 31, 32] {
            let t = l
                .tiles
                .iter()
                .find(|t| t.kind == TileKind::Session(id))
                .unwrap_or_else(|| panic!("{id} has its own tile"));
            assert!(t.rect.w >= PINNED_MIN_W_PX - 0.01, "{id} w {}", t.rect.w);
            assert!(t.rect.h >= PINNED_MIN_H_PX - 0.01, "{id} h {}", t.rect.h);
        }
        let merged: Vec<&Vec<u32>> = l
            .tiles
            .iter()
            .filter_map(|t| match &t.kind {
                TileKind::Merged(ids) => Some(ids),
                _ => None,
            })
            .collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].len(), 10, "only the unpinned small ones merge");
        for t in &l.tiles {
            assert!(bounds.contains(&t.rect, 0.01), "{:?}", t.rect);
        }
        for (i, a) in l.tiles.iter().enumerate() {
            for b in &l.tiles[i + 1..] {
                assert!(a.rect.overlap(&b.rect) < 0.01, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn layout_pinned_without_pins_is_the_plain_layout() {
        let bounds = Rect::new(0.0, 0.0, 760.0, 466.0);
        let items = tipico();
        assert_eq!(
            layout_pinned(&items, bounds, &|_| false),
            layout(&items, bounds, true)
        );
    }

    #[test]
    fn nearest_moves_to_the_adjacent_tile_in_each_direction() {
        // 0 | 1      0 spans the left column; 1 top right, 2 bottom right.
        // 0 | 2
        let r = [
            Rect::new(0.0, 0.0, 50.0, 100.0),
            Rect::new(50.0, 0.0, 50.0, 40.0),
            Rect::new(50.0, 40.0, 50.0, 60.0),
        ];
        assert_eq!(
            nearest(&r, 0, Dir::Right),
            Some(2),
            "both overlap with no gap: the closer centre"
        );
        assert_eq!(nearest(&r, 1, Dir::Left), Some(0));
        assert_eq!(nearest(&r, 1, Dir::Down), Some(2));
        assert_eq!(nearest(&r, 2, Dir::Up), Some(1));
        assert_eq!(nearest(&r, 0, Dir::Left), None, "edge");
        assert_eq!(nearest(&r, 1, Dir::Up), None);
        assert_eq!(nearest(&r, 1, Dir::Right), None);
        // No tile overlaps across the direction: closest centre past the edge.
        let r = [
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(20.0, 50.0, 10.0, 10.0),
            Rect::new(20.0, 15.0, 10.0, 10.0),
        ];
        assert_eq!(nearest(&r, 0, Dir::Right), Some(2));
        assert_eq!(nearest(&r, 0, Dir::Down), Some(2));
    }

    #[test]
    fn nearest_reaches_every_tile_of_the_tipico_map() {
        let l = layout(&tipico(), MAP, true);
        let rects: Vec<Rect> = l.tiles.iter().map(|t| t.rect).collect();
        // Breadth-first over the four arrows from the first tile.
        let mut seen = vec![false; rects.len()];
        let mut queue = vec![0];
        seen[0] = true;
        while let Some(i) = queue.pop() {
            for d in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
                if let Some(j) = nearest(&rects, i, d)
                    && !seen[j]
                {
                    seen[j] = true;
                    queue.push(j);
                }
            }
        }
        assert!(seen.iter().all(|s| *s), "unreachable tiles: {seen:?}");
    }

    // ---- shared drift rule and names ------------------------------------------

    fn w3(a: f64) -> Vec<(u32, f64)> {
        vec![(1, a), (2, 100.0), (3, 100.0)]
    }

    #[test]
    fn drift_waits_for_20_percent_and_has_no_timer_of_its_own() {
        let mut st = DriftStability::default();
        assert!(st.should_relayout(&w3(100.0), 0), "first call");
        assert!(!st.should_relayout(&w3(100.0), 120_000), "no timer");
        assert!(!st.should_relayout(&w3(125.0), 200_000), "+15 %");
        let mut fresh = DriftStability::default();
        assert!(fresh.should_relayout(&w3(100.0), 0));
        assert!(!fresh.should_relayout(&w3(140.0), 20_000), "inside 30 s");
        assert!(fresh.should_relayout(&w3(140.0), 30_000), "+24 % at 30 s");
        assert!(!fresh.should_relayout(&w3(140.0), 90_000), "new base");
    }

    #[test]
    fn a_size_order_flip_under_the_drift_is_no_reason() {
        let mut st = DriftStability::default();
        let a = [(1, 100.0), (2, 105.0), (3, 300.0)];
        let b = [(1, 106.0), (2, 99.0), (3, 300.0)];
        let order = |w: &[(u32, f64)]| {
            let mut v = w.to_vec();
            v.sort_by(|x, y| y.1.total_cmp(&x.1));
            v.iter().map(|x| x.0).collect::<Vec<_>>()
        };
        assert_ne!(order(&a), order(&b), "the order flips");
        assert!(st.should_relayout(&a, 0));
        assert!(!st.should_relayout(&b, 60_000));
    }

    #[test]
    fn start_end_invalidate_and_a_clock_going_back() {
        let mut st = DriftStability::default();
        assert!(st.should_relayout(&w3(100.0), 0));
        let mut four = w3(100.0);
        four.push((4, 100.0));
        assert!(!st.should_relayout(&four, 10_000), "start inside 30 s");
        assert!(st.should_relayout(&four, 30_000), "start");
        assert!(!st.should_relayout(&w3(100.0), 50_000), "end inside 30 s");
        assert!(st.should_relayout(&w3(100.0), 60_000), "end");
        st.invalidate();
        assert!(st.should_relayout(&w3(100.0), 61_000), "a resize");
        assert!(st.should_relayout(&w3(100.0), 1_000), "clock went back");
        st.rebase(&w3(100.0), 5_000);
        assert!(!st.should_relayout(&w3(100.0), 6_000), "rebase records");
    }

    #[test]
    fn the_name_font_is_12_below_96_px_and_13_from_there() {
        assert_eq!(name_px(36.0), 12.0);
        assert_eq!(name_px(95.0), 12.0);
        assert_eq!(name_px(96.0), 13.0);
        assert_eq!(NAME_MIN_W_PX, 36.0);
    }

    #[test]
    fn a_tile_36_px_wide_shows_its_name_on_its_own_row() {
        let n = "kiln-cache-layer";
        // 35 px: glyph only. 36 px: the name, across the whole width.
        assert_eq!(label_fit(n, 35.0, 80.0, 56.0, seven), LabelFit::Glyph);
        assert_eq!(
            label_fit(n, 36.0, 80.0, 56.0, seven),
            LabelFit::Stacked { value: false },
            "the value (56) does not fit in 20"
        );
        assert_eq!(
            label_fit(n, 36.0, 40.0, 56.0, seven),
            LabelFit::Stacked { value: false },
            "glyph row plus name row is 40 px"
        );
        assert_eq!(
            label_fit(n, 36.0, 39.0, 56.0, seven),
            LabelFit::Glyph,
            "no room for the name row"
        );
    }

    #[test]
    fn a_stacked_tile_adds_the_value_when_it_fits_beside_a_third_row() {
        let n = "ledgerauthrefactor";
        assert_eq!(
            label_fit(n, 100.0, 60.0, 56.0, seven),
            LabelFit::Stacked { value: true },
            "first word too long for the first line: stack, value fits in 84"
        );
        assert_eq!(
            label_fit(n, 100.0, 50.0, 56.0, seven),
            LabelFit::Stacked { value: false },
            "no third row under 58 px"
        );
        assert_eq!(
            label_fit(n, 60.0, 60.0, 56.0, seven),
            LabelFit::Stacked { value: false },
            "value 56 needs 72"
        );
    }
}
