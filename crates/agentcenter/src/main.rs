//! `agentcenter`: monitor de recursos por sessao de agente de IA, em GTK4 +
//! libadwaita. A UI nunca le o sistema de arquivos do kernel: tudo vem do
//! sampler do core, numa thread propria, um `Snapshot` imutavel por tick.

mod actions_ui;
mod app;
mod context_gauge;
mod detail;
mod feed;
mod format;
mod graph;
mod machine_history;
mod machine_strip;
mod map_page;
mod measure;
mod memory_map;
mod menu;
mod motion;
mod prefs;
mod process_table;
mod reclaim_page;
mod renderer;
mod session_gauge;
mod session_list;
mod session_object;
mod spend;
mod state_glyph;
mod timeline;
// Wired by the window; TokenInput::items is still unused (token_map.rs is not this card's).
#[allow(dead_code)]
mod token_map;
mod treemap;
mod window;

fn main() -> gtk::glib::ExitCode {
    let start = std::time::Instant::now();
    renderer::apply_default();
    app::run(start)
}
