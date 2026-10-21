//! `agentcenter`: monitor de recursos por sessao de agente de IA, em GTK4 +
//! libadwaita. A UI nunca le o sistema de arquivos do kernel: tudo vem do
//! sampler do core, numa thread propria, um `Snapshot` imutavel por tick.

mod actions_ui;
mod app;
mod dashboard;
mod detail;
mod feed;
mod format;
mod graph;
mod machine_header;
mod measure;
mod prefs;
mod process_table;
mod reclaim_page;
mod renderer;
mod session_list;
mod session_object;
mod window;

fn main() -> gtk::glib::ExitCode {
    let start = std::time::Instant::now();
    renderer::apply_default();
    app::run(start)
}
