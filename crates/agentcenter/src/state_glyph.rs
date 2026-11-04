//! State glyph shared by the list row and the detail header: busy disc,
//! waiting triangle, idle ring, multiplexer 2x2 grid. Drawn in the node's CSS
//! `color`, so the tone class (`glyph.busy|waiting|idle|mux|unknown`) picks the
//! token and the theme swaps it.

use crate::format::Group;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use std::cell::Cell;

/// Shape of the state glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    Disc,
    Triangle,
    #[default]
    Ring,
    Grid,
}

/// Glyph shape and tone class (the CSS class that picks the fill token) of a
/// group rank; an unknown state is a ring too.
pub fn glyph(group: u32) -> (Shape, &'static str) {
    match Group::from_rank(group) {
        Group::Busy => (Shape::Disc, "busy"),
        Group::Blocked => (Shape::Triangle, "waiting"),
        Group::Idle => (Shape::Ring, "idle"),
        Group::Multiplexer => (Shape::Grid, "mux"),
        Group::Unknown => (Shape::Ring, "unknown"),
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct StateGlyph {
        pub shape: Cell<Shape>,
        pub px: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StateGlyph {
        const NAME: &'static str = "AgentCenterStateGlyph";
        type Type = super::StateGlyph;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("glyph");
        }
    }

    impl ObjectImpl for StateGlyph {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_can_target(false);
            obj.set_focusable(false);
            // The state is spoken by the row's label or the detail's pill.
            obj.set_accessible_role(gtk::AccessibleRole::Presentation);
            obj.set_valign(gtk::Align::Center);
        }
    }

    impl WidgetImpl for StateGlyph {
        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let px = self.px.get();
            (px, px, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let s = w.min(h);
            if s <= 0.0 {
                return;
            }
            let color = obj.color();
            let (x0, y0) = ((w - s) / 2.0, (h - s) / 2.0);
            let center = graphene::Point::new(x0 + s / 2.0, y0 + s / 2.0);
            let path = gsk::PathBuilder::new();
            match self.shape.get() {
                Shape::Disc => {
                    path.add_circle(&center, s / 2.0 - 1.0);
                    snapshot.append_fill(&path.to_path(), gsk::FillRule::Winding, &color);
                }
                Shape::Ring => {
                    // 1.5 px reads as a ring at 12 px; the 16 px header needs 2.
                    let stroke = if s >= 16.0 { 2.0 } else { 1.5 };
                    path.add_circle(&center, s / 2.0 - 1.75);
                    snapshot.append_stroke(&path.to_path(), &gsk::Stroke::new(stroke), &color);
                }
                Shape::Triangle => {
                    path.move_to(x0 + s / 2.0, y0 + 0.5);
                    path.line_to(x0 + s - 0.5, y0 + s - 1.0);
                    path.line_to(x0 + 0.5, y0 + s - 1.0);
                    path.close();
                    snapshot.append_fill(&path.to_path(), gsk::FillRule::Winding, &color);
                }
                Shape::Grid => {
                    let gap = 2.0;
                    let cell = (s - gap) / 2.0;
                    for (i, j) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                        let r = graphene::Rect::new(
                            x0 + i * (cell + gap),
                            y0 + j * (cell + gap),
                            cell,
                            cell,
                        );
                        snapshot.append_color(&color, &r);
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    /// State glyph. CSS: `glyph.busy|waiting|idle|mux|unknown`.
    pub struct StateGlyph(ObjectSubclass<imp::StateGlyph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StateGlyph {
    /// A glyph in a `px` square box.
    pub fn new(px: i32) -> Self {
        let g: Self = glib::Object::new();
        g.imp().px.set(px);
        g
    }

    pub fn shape(&self) -> Shape {
        self.imp().shape.get()
    }

    /// Shape and tone of the group rank `group`; repaints only on a change.
    pub fn show(&self, group: u32) {
        let (shape, tone) = glyph(group);
        if self.imp().shape.replace(shape) != shape {
            self.queue_draw();
        }
        if !self.has_css_class(tone) {
            self.set_css_classes(&[tone]);
        }
    }
}
