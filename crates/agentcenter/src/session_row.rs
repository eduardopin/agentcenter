//! Linha rica de uma sessao (docs/DESIGN.md secao 5) e o cabecalho de secao.
//!
//! A linha e montada uma vez no `setup` da fabrica e ligada ao item por
//! expressao: o GTK troca o [`SessionObject`] quando recicla a linha, e cada
//! `notify` de propriedade atualiza so o rotulo dela. Nenhum widget nasce por
//! tick. Tres linhas de texto:
//!
//! ```text
//! [icone] nome (elipse no fim)                       [Busy] [HANDOFF]
//!         agente · provider · modelo   ~/cwd (elipse no inicio)   w4:p16   burn · cache · handoff
//!         CPU ▮▯ 16%   728 MiB + 174 MiB swap   ctx ▮▮▮| 86%   tools 2   sub —   I/O-bound
//! ```
//!
//! As partes opcionais saem por largura com a [`Density`] da lista, que os
//! breakpoints da janela mudam: um objeto so, ligado a cada linha no setup.

use crate::actions_ui;
use crate::format::{Group, rec_class};
use crate::session_object::SessionObject;
use agentcenter_core::actions::Target;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{glib, pango};

mod imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Density)]
    pub struct Density {
        /// Sem o bloco do cockpit na segunda linha nem subagentes.
        #[property(get, set)]
        compact: Cell<bool>,
        /// So o essencial: sem barras, swap, pane e tool calls.
        #[property(get, set)]
        narrow: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Density {
        const NAME: &'static str = "AgentCenterRowDensity";
        type Type = super::Density;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Density {}
}

glib::wrapper! {
    /// Quanto da linha cabe na largura atual.
    pub struct Density(ObjectSubclass<imp::Density>);
}

impl Default for Density {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Largura (px) das barras de CPU e de contexto.
pub const METER_WIDTH: i32 = 48;
/// Marcas da barra de contexto: compactar (50 %) e handoff (90 %, o padrao de
/// `SQUAD_CTX_PCT` no cockpit).
pub const CTX_MARKS: [f64; 2] = [0.5, 0.9];

fn bind(item: &gtk::ListItem, prop: &str, target: &impl IsA<glib::Object>, target_prop: &str) {
    item.property_expression("item")
        .chain_property::<SessionObject>(prop)
        .bind(target, target_prop, gtk::Widget::NONE);
}

/// Liga `target_prop` a uma funcao de uma propriedade `u32` do item.
fn bind_u32<R: glib::value::ValueType + glib::value::ToValue>(
    item: &gtk::ListItem,
    prop: &str,
    target: &impl IsA<glib::Object>,
    target_prop: &str,
    f: impl Fn(u32) -> R + 'static,
) {
    item.property_expression("item")
        .chain_property::<SessionObject>(prop)
        .chain_closure_with_callback(move |args| f(args[1].get::<u32>().unwrap_or_default()))
        .bind(target, target_prop, gtk::Widget::NONE);
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// `visible` = nao `flag` da densidade.
fn hide_when(density: &Density, flag: &str, w: &impl IsA<gtk::Widget>) {
    density
        .bind_property(flag, w.as_ref(), "visible")
        .invert_boolean()
        .sync_create()
        .build();
}

fn show_when(density: &Density, flag: &str, w: &impl IsA<gtk::Widget>) {
    density
        .bind_property(flag, w.as_ref(), "visible")
        .sync_create()
        .build();
}

/// Barra fina com trilho; `marks` sao entalhes na cor da superficie.
fn meter(marks: &[f64], hot_from: Option<f64>) -> (gtk::Overlay, gtk::LevelBar) {
    let bar = gtk::LevelBar::new();
    bar.set_min_value(0.0);
    bar.set_max_value(1.0);
    for name in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(name));
    }
    if let Some(hot) = hot_from {
        // Classe do bloco cheio pelo valor: ate `hot` normal, depois quente.
        bar.add_offset_value("meter-ok", hot - 1e-6);
        bar.add_offset_value("meter-hot", 1.0);
    }
    bar.set_size_request(METER_WIDTH, -1);
    bar.set_valign(gtk::Align::Center);
    bar.add_css_class("meter");
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&bar));
    overlay.set_valign(gtk::Align::Center);
    for m in marks {
        let tick = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tick.add_css_class("meter-tick");
        tick.set_halign(gtk::Align::Start);
        tick.set_margin_start((m * f64::from(METER_WIDTH)).round() as i32 - 1);
        tick.set_size_request(2, -1);
        tick.set_can_target(false);
        overlay.add_overlay(&tick);
    }
    (overlay, bar)
}

/// Pilula de estado: preenchida em busy e blocked, contorno no resto.
pub fn state_classes(group: u32) -> Vec<String> {
    let state = match Group::from_rank(group) {
        Group::Busy => "busy",
        Group::Blocked => "blocked",
        Group::Idle => "idle",
        Group::Unknown | Group::Multiplexer => "unknown",
    };
    vec!["ac-pill".into(), state.into()]
}

fn icon_classes(group: u32) -> Vec<String> {
    let tone = match Group::from_rank(group) {
        Group::Busy => "busy",
        Group::Blocked => "blocked",
        _ => "quiet",
    };
    vec!["session-icon".into(), tone.into()]
}

fn is_agent(g: u32) -> bool {
    Group::from_rank(g) != Group::Multiplexer
}

/// Monta a linha no item e liga cada parte ao [`SessionObject`] dele.
pub fn setup(item: &gtk::ListItem, density: &Density) {
    let icon = gtk::Image::new();
    icon.set_pixel_size(16);
    icon.set_valign(gtk::Align::Start);
    icon.set_margin_top(2);
    icon.set_accessible_role(gtk::AccessibleRole::Presentation);
    bind_u32(item, "group", &icon, "icon-name", move |g| {
        if is_agent(g) {
            "utilities-terminal-symbolic".to_string()
        } else {
            "view-grid-symbolic".to_string()
        }
    });
    bind_u32(item, "group", &icon, "css-classes", icon_classes);

    // Linha 1: nome, estado, recomendacao.
    let name = label(&["session-name"]);
    name.set_ellipsize(pango::EllipsizeMode::End);
    name.set_hexpand(true);
    bind(item, "name", &name, "label");
    bind(item, "name", &name, "tooltip-text");
    let state = label(&[]);
    state.set_valign(gtk::Align::Center);
    bind(item, "status", &state, "label");
    bind_u32(item, "group", &state, "css-classes", state_classes);
    bind_u32(item, "group", &state, "visible", is_agent);
    let rec = label(&[]);
    rec.set_valign(gtk::Align::Center);
    bind(item, "rec-text", &rec, "label");
    bind(item, "rec-tooltip", &rec, "tooltip-text");
    bind_u32(item, "rec", &rec, "css-classes", |r| {
        vec!["ac-pill".to_string(), "rec".into(), rec_class(r).into()]
    });
    bind_u32(item, "group", &rec, "visible", is_agent);
    // Acoes da sessao (C12): o menu le a sessao da linha na hora de abrir,
    // porque o GTK recicla a linha para outra sessao.
    let weak = item.downgrade();
    let actions = actions_ui::menu_button("Session actions", move || {
        let o = weak.upgrade()?.item().and_downcast::<SessionObject>()?;
        Some((Target::Session(o.pid()), o.name()))
    });
    bind_u32(item, "group", &actions, "visible", is_agent);
    let line1 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line1.append(&name);
    line1.append(&state);
    line1.append(&rec);
    line1.append(&actions);

    // Linha 2: identidade e o bloco do cockpit.
    let meta = label(&["session-meta"]);
    meta.set_ellipsize(pango::EllipsizeMode::End);
    bind(item, "meta", &meta, "label");
    bind(item, "meta", &meta, "tooltip-text");
    let cwd = label(&["session-meta", "monospace"]);
    // O fim do caminho e o que distingue uma sessao da outra.
    cwd.set_ellipsize(pango::EllipsizeMode::Start);
    cwd.set_hexpand(true);
    bind(item, "cwd", &cwd, "label");
    bind(item, "cwd", &cwd, "tooltip-text");
    let place = label(&["session-meta", "numeric"]);
    bind(item, "place", &place, "label");
    hide_when(density, "narrow", &place);
    let cockpit = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    for (text, tip) in [
        ("burn-text", Some("burn-tooltip")),
        ("cache-text", Some("cache-tooltip")),
        ("handoff-text", Some("handoff-tooltip")),
    ] {
        let l = label(&["session-meta", "numeric"]);
        bind(item, text, &l, "label");
        if let Some(tip) = tip {
            bind(item, tip, &l, "tooltip-text");
        }
        cockpit.append(&l);
    }
    hide_when(density, "compact", &cockpit);
    let cockpit_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cockpit_slot.append(&cockpit);
    bind_u32(item, "group", &cockpit_slot, "visible", is_agent);
    let line2 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line2.append(&meta);
    line2.append(&cwd);
    line2.append(&place);
    line2.append(&cockpit_slot);

    // Linha 3: custo e atividade.
    let cpu = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let cpu_caption = label(&["session-caption"]);
    cpu_caption.set_label("CPU");
    let (cpu_meter, cpu_bar) = meter(&[], None);
    bind(item, "cpu-frac", &cpu_bar, "value");
    hide_when(density, "narrow", &cpu_meter);
    let cpu_text = label(&["session-value", "numeric"]);
    bind(item, "cpu-text", &cpu_text, "label");
    cpu.append(&cpu_caption);
    cpu.append(&cpu_meter);
    cpu.append(&cpu_text);
    bind(item, "cpu-tooltip", &cpu, "tooltip-text");

    let mem_full = label(&["session-value", "numeric"]);
    bind(item, "mem-text", &mem_full, "label");
    hide_when(density, "narrow", &mem_full);
    let mem_short = label(&["session-value", "numeric"]);
    bind(item, "pss-text", &mem_short, "label");
    bind(item, "mem-text", &mem_short, "tooltip-text");
    show_when(density, "narrow", &mem_short);

    let agent_part = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let ctx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let ctx_caption = label(&["session-caption"]);
    ctx_caption.set_label("ctx");
    let (ctx_meter, ctx_bar) = meter(&CTX_MARKS, Some(CTX_MARKS[1]));
    bind(item, "ctx-frac", &ctx_bar, "value");
    hide_when(density, "narrow", &ctx_meter);
    let ctx_text = label(&["session-value", "numeric"]);
    bind(item, "ctx-text", &ctx_text, "label");
    ctx.append(&ctx_caption);
    ctx.append(&ctx_meter);
    ctx.append(&ctx_text);
    ctx.set_tooltip_text(Some(
        "Context window used; marks at 50 % (compact) and 90 % (handoff)",
    ));
    let tools = label(&["session-value", "numeric"]);
    bind(item, "tools-text", &tools, "label");
    bind(item, "tools-tooltip", &tools, "tooltip-text");
    hide_when(density, "narrow", &tools);
    let sub = label(&["session-value", "numeric"]);
    bind(item, "sub-text", &sub, "label");
    bind(item, "sub-tooltip", &sub, "tooltip-text");
    hide_when(density, "compact", &sub);
    agent_part.append(&ctx);
    agent_part.append(&tools);
    agent_part.append(&sub);
    bind_u32(item, "group", &agent_part, "visible", is_agent);

    let io = label(&["session-flag"]);
    bind(item, "io-text", &io, "label");
    bind(item, "io-tooltip", &io, "tooltip-text");

    let line3 = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    line3.append(&cpu);
    line3.append(&mem_full);
    line3.append(&mem_short);
    line3.append(&agent_part);
    line3.append(&io);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    body.set_hexpand(true);
    body.append(&line1);
    body.append(&line2);
    body.append(&line3);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.add_css_class("session-row");
    root.append(&icon);
    root.append(&body);
    item.set_child(Some(&root));
    // O leitor de tela ouve a linha inteira numa frase, com recomendacao e ctx.
    bind(item, "a11y-label", item, "accessible-label");
}

/// Cabecalho de secao: titulo e contagem, `Blocked · 3`. O titulo vem de
/// `title(primeiro item)`; a contagem e a do proprio `GtkListHeader`.
pub fn setup_header(
    header: &gtk::ListHeader,
    title: impl Fn(&SessionObject) -> (String, bool) + 'static,
) {
    let l = label(&["session-section"]);
    l.set_accessible_role(gtk::AccessibleRole::Heading);
    header.set_child(Some(&l));
    let refresh = move |h: &gtk::ListHeader| {
        let Some(obj) = h.item().and_downcast::<SessionObject>() else {
            return;
        };
        let (t, alert) = title(&obj);
        let text = format!("{t} · {}", h.n_items());
        if l.label() != text {
            l.set_label(&text);
        }
        if alert != l.has_css_class("alert") {
            if alert {
                l.add_css_class("alert");
            } else {
                l.remove_css_class("alert");
            }
        }
    };
    let r = std::rc::Rc::new(refresh);
    let r2 = r.clone();
    header.connect_item_notify(move |h| r(h));
    header.connect_n_items_notify(move |h| r2(h));
}
