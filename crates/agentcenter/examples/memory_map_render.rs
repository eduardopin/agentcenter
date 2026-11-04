//! Visual proof of the memory map widget alone: renders the tipico-40
//! fixture at 760x466 in light and dark, normal and reclaim mode, to
//! `target/design/impl/memory-map/*.png`. Needs a display (Broadway works).
#![allow(dead_code)]
use adw::prelude::*;
use gtk::{gdk, glib};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "../src/format.rs"]
mod format;
#[path = "../src/memory_map.rs"]
mod memory_map;
#[path = "../src/motion.rs"]
mod motion;
#[path = "../src/treemap.rs"]
mod treemap;

use memory_map::{MapInput, MemoryMap};
use treemap::TileState;

const SIZE: (i32, i32) = (760, 466);

fn settle(ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    let context = glib::MainContext::default();
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn save_png(window: &gtk::Window, path: &Path) -> Result<(), String> {
    let (w, h) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let node = snapshot.to_node().ok_or("empty window")?;
    let renderer = window.renderer().ok_or("window has no renderer")?;
    let texture: gdk::Texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

fn main() {
    adw::init().expect("GTK display");
    let css = gtk::CssProvider::new();
    css.load_from_string(concat!(
        include_str!("../resources/tokens.css"),
        include_str!("../resources/memory_map.css"),
    ));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/snapshots/tipico-40.json");
    let snap = serde_json::from_str(&std::fs::read_to_string(fixture).expect("fixture"))
        .expect("parse fixture");
    let input = MapInput::from_snapshot(&snap);
    // The three largest idle sessions, checked as in the reclaim mock-up.
    let mut idle: Vec<_> = input
        .sessions
        .iter()
        .filter(|s| s.state == TileState::Idle)
        .collect();
    idle.sort_by_key(|s| std::cmp::Reverse(s.bytes()));
    let checked: Vec<u32> = idle.iter().take(3).map(|s| s.pid).collect();

    let out: PathBuf = root.join("../../target/design/impl/memory-map");
    std::fs::create_dir_all(&out).expect("output dir");
    let style = adw::StyleManager::default();
    for (theme, scheme) in [
        ("light", adw::ColorScheme::ForceLight),
        ("dark", adw::ColorScheme::ForceDark),
    ] {
        style.set_color_scheme(scheme);
        // Our provider has its own prefers-color-scheme (see app.rs).
        css.set_prefers_color_scheme(if style.is_dark() {
            gtk::InterfaceColorScheme::Dark
        } else {
            gtk::InterfaceColorScheme::Light
        });
        for reclaim in [false, true] {
            let map = MemoryMap::new();
            map.set_input(input.clone());
            map.set_reclaim(reclaim);
            if reclaim {
                map.set_checked(&checked);
            } else {
                map.set_selected(input.sessions.first().map(|s| s.pid));
            }
            let win = gtk::Window::new();
            win.set_decorated(false);
            win.set_default_size(SIZE.0, SIZE.1);
            win.set_child(Some(&map));
            win.present();
            settle(400);
            let mode = if reclaim { "reclaim" } else { "main" };
            let path = out.join(format!("{mode}-760-{theme}.png"));
            let mut result = save_png(&win, &path);
            for _ in 0..20 {
                if result.is_ok() {
                    break;
                }
                win.queue_draw();
                settle(200);
                result = save_png(&win, &path);
            }
            result.expect("render");
            println!("{}", path.display());
            win.destroy();
        }
    }
}
