//! Linha rica de uma sessao (docs/DESIGN.md secoes 5 e 9) e o cabecalho de
//! secao.
//!
//! A linha e montada uma vez no `setup` da fabrica e ligada ao item por
//! expressao: o GTK troca o [`SessionObject`] quando recicla a linha, e cada
//! `notify` de propriedade atualiza so a parte dela. Nenhum widget nasce por
//! tick; barra com valor igual nao realoca nem repinta ([`Rule::set`]).
//!
//! ```text
//! [icone] nome (elipse no fim)       [Busy] [HANDOFF]  ctx ▮▮▮|▮| 92%  burn ▮▮| 43%  [⋮]
//!         agente · provider · modelo   ~/cwd (elipse no inicio)   w4:p16   burn · cache · handoff
//!         CPU ▮▯ 16%   PSS ▮▯ 728 MiB   Swap ▮▯ 174 MiB   tools 2   sub —   I/O-bound
//! ```
//!
//! Na densidade estreita (lista abaixo de 600 px) contexto e burn descem para
//! o fim da linha 2 em regua compacta, com as mesmas marcas; CPU, PSS e Swap
//! ficam em texto:
//!
//! ```text
//! [icone] nome (elipse)                          [Busy] [HANDOFF]  [⋮]
//!         agente · prov…  …/cwd       ctx ▮|▮| 92%  burn ▮| 43%
//!         CPU 16%   728 MiB   I/O-bound
//! ```
//!
//! As barras sao a regua de gauge.rs em tamanho de linha (`rule.mini.row`):
//! trilho, preenchimento ambar (o consumo da propria sessao), marcas em tinta.
//! Contexto e burn ficam ao lado da recomendacao porque sao as duas contas que
//! a produzem; as marcas sao os limiares do vigia do ambiente
//! ([`process_thresholds`]). As partes opcionais saem por largura com a
//! [`Density`] da lista, que os breakpoints da janela mudam: um objeto so,
//! ligado a cada linha no setup.

use crate::actions_ui;
use crate::format::{
    Group, UNAVAILABLE, ctx_hot, ctx_marks, gauge_frac, pct_short, process_thresholds, rec_class,
};
use crate::machine_header::gauge::{Fill, Rule};
use crate::session_object::SessionObject;
use agentcenter_core::actions::Target;
use agentcenter_core::cockpit::Thresholds;
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

/// Largura (px) das barras de contexto e burn; a da CPU, PSS e Swap e
/// [`MINI_GAUGE_PX`]. A altura vem do CSS (`rule.mini.row`).
pub const GAUGE_PX: i32 = 40;
pub const MINI_GAUGE_PX: i32 = 28;

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

/// Na estreita o ar entre grupos cai de 12 para 8 px: a linha 3 cabe nos
/// 360 px da janela minima com contexto e burn em texto.
fn tighten_when_narrow(density: &Density, b: &gtk::Box) {
    density
        .bind_property("narrow", b, "spacing")
        .transform_to(|_, narrow: bool| Some(if narrow { 8 } else { 12 }))
        .sync_create()
        .build();
}

fn show_when(density: &Density, flag: &str, w: &impl IsA<gtk::Widget>) {
    density
        .bind_property(flag, w.as_ref(), "visible")
        .sync_create()
        .build();
}

/// Regua de linha com rotulo acessivel e marcas em % da capacidade.
fn row_rule(label: &str, marks_pct: &[f64], mini: bool) -> Rule {
    let marks: Vec<f64> = marks_pct.iter().map(|m| m / 100.0).collect();
    let rule = Rule::new(label, &marks);
    // `mini` tira a largura natural de 160 px da regua grande; `row` e o
    // tamanho de linha em summary.css.
    rule.add_css_class("mini");
    rule.add_css_class("row");
    rule.set_size_request(if mini { MINI_GAUGE_PX } else { GAUGE_PX }, -1);
    rule.set_valign(gtk::Align::Center);
    // Nasce sem leitura: o estado inicial do Rule ja e `None`, entao `set`
    // nao mudaria nada; a classe do estado ausente entra aqui.
    rule.add_css_class("absent");
    rule.set_value_text(UNAVAILABLE);
    rule
}

/// Liga a regua a uma propriedade em % do item (-1 = ausente): a expressao
/// observa o item e a propriedade, e a regua so muda quando o valor muda. O
/// preenchimento e todo "agents" (ambar): o consumo e da propria sessao.
fn watch_rule(
    item: &gtk::ListItem,
    prop: &str,
    rule: &Rule,
    alert: impl Fn(f64) -> bool + 'static,
) {
    let expr = item
        .property_expression("item")
        .chain_property::<SessionObject>(prop);
    let eval = expr.clone();
    let weak = rule.downgrade();
    expr.watch(Some(rule), move || {
        let Some(rule) = weak.upgrade() else { return };
        let pct = eval.evaluate_as::<f64, gtk::Widget>(None).unwrap_or(-1.0);
        let fill = gauge_frac(pct).map(|f| Fill {
            agents: f,
            rest: 0.0,
            overflow: pct > 100.0,
        });
        rule.set(fill, fill.is_some() && alert(pct));
        rule.set_value_text(&fill.map_or_else(|| UNAVAILABLE.to_string(), |_| pct_short(pct)));
    });
}

/// Legenda, regua e valor: `ctx ▮▮| 86%`. O valor tem largura fixa em
/// caracteres para as reguas ficarem em coluna de uma linha para a outra.
fn gauge_group(caption: &str, rule: &Rule, value: &gtk::Label, chars: i32) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let c = label(&["session-caption"]);
    c.set_label(caption);
    b.append(&c);
    b.append(rule);
    value.set_width_chars(chars);
    b.append(value);
    b
}

/// Contexto e burn, as duas contas da recomendacao, com as marcas do vigia.
/// `mini` e a forma da densidade estreita: regua de [`MINI_GAUGE_PX`] e ar
/// de 4 px entre legenda, regua e valor.
fn ctx_burn(item: &gtk::ListItem, th: &Thresholds, mini: bool) -> gtk::Box {
    // Contexto: marcas no piso da releitura e no handoff; carmim no handoff.
    let ctx_rule = row_rule("Context window", &ctx_marks(th), mini);
    let hot_th = *th;
    watch_rule(item, "ctx", &ctx_rule, move |p| ctx_hot(&hot_th, p));
    let ctx_value = label(&["session-value", "numeric"]);
    bind(item, "ctx-text", &ctx_value, "label");
    let ctx = gauge_group("ctx", &ctx_rule, &ctx_value, 4);
    bind(item, "ctx-tooltip", &ctx, "tooltip-text");
    // Burn: % do orcamento da sessao, marca onde o piso chega ao minimo.
    let burn_rule = row_rule(
        "Session budget burned",
        &[f64::from(th.burn_full_pct)],
        mini,
    );
    watch_rule(item, "burn-pct", &burn_rule, |_| false);
    let burn_value = label(&["session-value", "numeric"]);
    bind(item, "burn-pct-text", &burn_value, "label");
    let burn = gauge_group("burn", &burn_rule, &burn_value, 4);
    bind(item, "burn-tooltip", &burn, "tooltip-text");
    let gauges = gtk::Box::new(gtk::Orientation::Horizontal, if mini { 8 } else { 12 });
    gauges.add_css_class("row-gauges");
    if mini {
        ctx.set_spacing(4);
        burn.set_spacing(4);
    }
    gauges.append(&ctx);
    gauges.append(&burn);
    gauges
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

/// Monta a linha no item com os limiares do vigia deste processo.
pub fn setup(item: &gtk::ListItem, density: &Density) {
    setup_with(item, density, &process_thresholds());
}

/// [`setup`] com limiares explicitos (testes com `SQUAD_*` alterados).
pub fn setup_with(item: &gtk::ListItem, density: &Density, th: &Thresholds) {
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

    // Linha 1: nome, estado, recomendacao e as duas contas que a produzem.
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

    let gauges = ctx_burn(item, th, false);
    hide_when(density, "narrow", &gauges);
    let gauges_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    gauges_slot.append(&gauges);
    bind_u32(item, "group", &gauges_slot, "visible", is_agent);

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
    line1.append(&gauges_slot);
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
        ("burn-text", "burn-tooltip"),
        ("cache-text", "cache-tooltip"),
        ("handoff-text", "handoff-tooltip"),
    ] {
        let l = label(&["session-meta", "numeric"]);
        bind(item, text, &l, "label");
        bind(item, tip, &l, "tooltip-text");
        cockpit.append(&l);
    }
    hide_when(density, "compact", &cockpit);
    let cockpit_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cockpit_slot.append(&cockpit);
    bind_u32(item, "group", &cockpit_slot, "visible", is_agent);
    // Estreita: contexto e burn descem para o fim da linha 2, compactos e
    // com as mesmas marcas. Na linha 1 nao cabem ao lado das pilulas em
    // 360 px; aqui o cwd e a identidade elidem e cedem a largura.
    let narrow_gauges = ctx_burn(item, th, true);
    show_when(density, "narrow", &narrow_gauges);
    let narrow_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    narrow_slot.append(&narrow_gauges);
    bind_u32(item, "group", &narrow_slot, "visible", is_agent);
    let line2 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line2.append(&meta);
    line2.append(&cwd);
    line2.append(&place);
    line2.append(&cockpit_slot);
    line2.append(&narrow_slot);
    tighten_when_narrow(density, &line2);

    // Linha 3: o custo para a maquina e a atividade.
    let cpu_rule = row_rule("CPU of one core", &[], true);
    watch_rule(item, "cpu", &cpu_rule, |_| false);
    hide_when(density, "narrow", &cpu_rule);
    let cpu_text = label(&["session-value", "numeric"]);
    bind(item, "cpu-text", &cpu_text, "label");
    let cpu = gauge_group("CPU", &cpu_rule, &cpu_text, 5);
    // Coluna fixa so com as barras; na estreita o texto ocupa o que precisa.
    density
        .bind_property("narrow", &cpu_text, "width-chars")
        .transform_to(|_, narrow: bool| Some(if narrow { 0 } else { 5 }))
        .sync_create()
        .build();
    bind(item, "cpu-tooltip", &cpu, "tooltip-text");

    let pss_rule = row_rule("PSS of RAM", &[], true);
    watch_rule(item, "pss-pct", &pss_rule, |_| false);
    let pss_text = label(&["session-value", "numeric"]);
    bind(item, "pss-text", &pss_text, "label");
    let pss = gauge_group("PSS", &pss_rule, &pss_text, 8);
    bind(item, "pss-tooltip", &pss, "tooltip-text");
    hide_when(density, "narrow", &pss);

    let swap_rule = row_rule("SwapPss of swap", &[], true);
    watch_rule(item, "swap-pct", &swap_rule, |_| false);
    let swap_text = label(&["session-value", "numeric"]);
    bind(item, "swap-text", &swap_text, "label");
    let swap = gauge_group("Swap", &swap_rule, &swap_text, 8);
    // Sem swap na maquina: so o texto "no swap", sem legenda nem trilho.
    if let Some(caption) = swap.first_child() {
        bind(item, "has-swap", &caption, "visible");
    }
    bind(item, "has-swap", &swap_rule, "visible");
    bind(item, "swap-tooltip", &swap, "tooltip-text");
    hide_when(density, "narrow", &swap);

    // Estreita: so o PSS, em texto (o mem-text inteiro no tooltip).
    let mem_short = label(&["session-value", "numeric"]);
    bind(item, "pss-text", &mem_short, "label");
    bind(item, "mem-text", &mem_short, "tooltip-text");
    show_when(density, "narrow", &mem_short);

    let agent_part = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    tighten_when_narrow(density, &agent_part);
    let tools = label(&["session-value", "numeric"]);
    bind(item, "tools-text", &tools, "label");
    bind(item, "tools-tooltip", &tools, "tooltip-text");
    hide_when(density, "narrow", &tools);
    let sub = label(&["session-value", "numeric"]);
    bind(item, "sub-text", &sub, "label");
    bind(item, "sub-tooltip", &sub, "tooltip-text");
    hide_when(density, "compact", &sub);
    agent_part.append(&tools);
    agent_part.append(&sub);
    bind_u32(item, "group", &agent_part, "visible", is_agent);

    let io = label(&["session-flag"]);
    bind(item, "io-text", &io, "label");
    bind(item, "io-tooltip", &io, "tooltip-text");

    let line3 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line3.append(&cpu);
    line3.append(&pss);
    line3.append(&swap);
    line3.append(&mem_short);
    line3.append(&agent_part);
    line3.append(&io);
    tighten_when_narrow(density, &line3);

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
    // O leitor de tela ouve a linha inteira numa frase, com recomendacao,
    // contexto e burn (ou "unavailable").
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Row, Scale};
    use crate::machine_header::gauge::mark_x;
    use agentcenter_core::{IoStallSource, Snapshot};

    fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut out = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(w) = child {
            out.extend(descendants(&w));
            child = w.next_sibling();
        }
        out
    }

    fn settle() {
        let ctx = glib::MainContext::default();
        for _ in 0..40 {
            while ctx.pending() {
                ctx.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
    }

    /// 40 sessoes (sem cockpit), nomes longos e o fixture do cockpit (com
    /// burn, pendente e sem dado), com os limiares do vigia alterados.
    fn objects(th: Thresholds) -> Vec<SessionObject> {
        let mut out = Vec::new();
        for name in ["tipico-40", "nomes-longos", "cockpit"] {
            let snap = fixture(name);
            let scale = Scale {
                thresholds: th,
                ..Scale::of(&snap.machine)
            };
            for s in &snap.sessions {
                let row = Row::from_session_in(s, IoStallSource::Delayacct, None, &scale);
                out.push(SessionObject::new(&row));
            }
        }
        out
    }

    fn rules_of(row: &gtk::Widget, label: &str) -> Vec<Rule> {
        descendants(row)
            .into_iter()
            .filter_map(|w| w.downcast::<Rule>().ok())
            .filter(|r| {
                r.parent()
                    .and_then(|p| p.first_child())
                    .and_downcast::<gtk::Label>()
                    .is_some_and(|c| c.label() == label)
            })
            .collect()
    }

    fn marks_x(rule: &Rule) -> Vec<i32> {
        descendants(rule.upcast_ref())
            .into_iter()
            .filter(|w| w.has_css_class("mark"))
            .filter_map(|m| m.compute_bounds(rule).map(|b| b.x().round() as i32))
            .collect()
    }

    #[test]
    fn forty_sessions_carry_the_vigia_marks_and_fit_each_density() {
        gtk::test_synced(|| {
            gtk::init().expect("GTK display (Broadway in CI)");
            let css = gtk::CssProvider::new();
            css.load_from_string(concat!(
                include_str!("../resources/tokens.css"),
                include_str!("../resources/summary.css"),
                include_str!("../resources/sessions.css"),
            ));
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().expect("display"),
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let th = Thresholds::from_env(|k| match k {
                "SQUAD_REREAD_PISO" => Some("50".into()),
                "SQUAD_CTX_PCT" => Some("80".into()),
                "SQUAD_BURN_CHEIO" => Some("30".into()),
                _ => None,
            });
            let objs = objects(th);
            assert!(objs.len() >= 54, "40 + long names + cockpit");
            let store = gtk::gio::ListStore::new::<SessionObject>();
            store.extend_from_slice(&objs);
            let density = Density::default();
            let factory = gtk::SignalListItemFactory::new();
            let d = density.clone();
            factory.connect_setup(move |_, item| {
                setup_with(item.downcast_ref().expect("list item"), &d, &th);
            });
            let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store))), Some(factory));
            view.add_css_class("session-rack");
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_child(Some(&view));
            let win = gtk::Window::new();
            win.set_default_size(1000, 4000);
            win.set_child(Some(&scroll));
            win.present();
            settle();

            let rows: Vec<gtk::Widget> = descendants(view.upcast_ref())
                .into_iter()
                .filter(|w| w.has_css_class("session-row"))
                .collect();
            assert!(rows.len() >= 40, "{} rows realized", rows.len());
            let (mut ctx_seen, mut ctx_drawn, mut burn_some, mut burn_absent) = (0, 0, 0, 0);
            for row in &rows {
                for ctx in rules_of(row, "ctx") {
                    let fill = ctx.state().fill;
                    // Quente a partir do SQUAD_CTX_PCT do ambiente (80).
                    let hot = fill.is_some_and(|f| f.total() >= 0.8 - 1e-9);
                    assert_eq!(ctx.has_css_class("alert"), hot);
                    assert_eq!(ctx.has_css_class("absent"), fill.is_none());
                    ctx_seen += 1;
                    if ctx.is_drawable() {
                        let w = ctx.width();
                        assert_eq!(w, GAUGE_PX);
                        // Marcas no piso da releitura (50) e no handoff (80).
                        assert_eq!(marks_x(&ctx), [mark_x(0.5, w), mark_x(0.8, w)]);
                        ctx_drawn += 1;
                    }
                }
                for burn in rules_of(row, "burn") {
                    if burn.is_drawable() {
                        assert_eq!(marks_x(&burn), [mark_x(0.3, burn.width())]);
                    }
                    match burn.state().fill {
                        Some(_) => burn_some += 1,
                        None => {
                            assert!(burn.has_css_class("absent"), "track only, never 0");
                            burn_absent += 1;
                        }
                    }
                }
                for cpu in rules_of(row, "CPU").iter().filter(|r| r.is_drawable()) {
                    assert_eq!(cpu.width(), MINI_GAUGE_PX);
                    assert!(marks_x(cpu).is_empty());
                }
            }
            assert!(
                ctx_seen >= 40 && ctx_drawn >= 8,
                "{ctx_seen} / {ctx_drawn} drawn"
            );
            assert!(
                burn_some >= 1 && burn_absent >= 38,
                "{burn_some} / {burn_absent}"
            );

            // Largura minima da linha em cada densidade: cabe no menor rack
            // dela (720 e 600 sp, e 360 px de janela), menos o recuo da
            // linha (12 px de cada lado). Nomes elidem; numeros nao.
            for (compact, narrow, rack) in
                [(false, false, 720), (true, false, 600), (true, true, 360)]
            {
                density.set_compact(compact);
                density.set_narrow(narrow);
                settle();
                // O Broadway sem navegador nao avanca o relogio de quadros:
                // a lista e alocada aqui para as barras que a densidade
                // mostrou ganharem largura antes da medida.
                scroll.allocate(scroll.width(), scroll.height(), -1, None);
                let widest = rows
                    .iter()
                    .map(|r| r.measure(gtk::Orientation::Horizontal, -1).0)
                    .max()
                    .unwrap_or(0);
                assert!(
                    widest <= rack - 24,
                    "compact={compact} narrow={narrow}: row needs {widest}px of {rack}"
                );
                // Barras desenhadas por legenda. Toda densidade mostra
                // contexto e burn com as marcas do vigia; a estreita os
                // mostra compactos e deixa CPU, PSS e Swap em texto.
                let drawn = |caption: &str| -> Vec<Rule> {
                    rows.iter()
                        .flat_map(|r| rules_of(r, caption))
                        .filter(|r| r.is_drawable())
                        .collect()
                };
                let (ctx, burn) = (drawn("ctx"), drawn("burn"));
                assert!(ctx.len() >= 8, "narrow={narrow}: {} ctx bars", ctx.len());
                assert_eq!(ctx.len(), burn.len(), "ctx and burn travel together");
                let px = if narrow { MINI_GAUGE_PX } else { GAUGE_PX };
                for r in &ctx {
                    assert_eq!(r.width(), px, "narrow={narrow}");
                    assert_eq!(marks_x(r), [mark_x(0.5, px), mark_x(0.8, px)]);
                }
                for r in &burn {
                    assert_eq!(r.width(), px, "narrow={narrow}");
                    assert_eq!(marks_x(r), [mark_x(0.3, px)]);
                }
                let machine = drawn("CPU").len() + drawn("PSS").len() + drawn("Swap").len();
                assert_eq!(machine == 0, narrow, "narrow keeps CPU/PSS/Swap as text");
            }
            win.destroy();
        });
    }
}
