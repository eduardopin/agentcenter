//! Regua (gauge linear) da faixa da maquina, docs/DESIGN.md secao 4.
//!
//! Uma regua so existe onde ha capacidade. O trilho e a capacidade inteira; a
//! parte cheia se divide em dois segmentos, a fatia dos agentes (ambar) e o
//! resto da maquina (ardosia), separados por 2 px; o que sobra e livre. Marcas
//! de limite (2 px) ficam por cima. Sem valor, so o trilho: nunca 0.
//!
//! O desenho e do GTK: o widget e o trilho (no CSS `rule`) e cada segmento e
//! uma marca sao filhos com no CSS proprio, posicionados aqui no
//! `size_allocate` e pintados pelo GtkSnapshot do widget a partir dos tokens
//! de summary.css. Nada de cairo nem cor no codigo: o tema troca so o CSS.
//! Valor igual ao anterior nao realoca nem repinta ([`RuleState::update`]).
//!
//! A parte sem GTK ([`Fill`], [`RuleState`], os formatadores) e testavel sem
//! display.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};

/// Folga entre os dois segmentos, em px (DESIGN.md secao 4).
pub const SEGMENT_GAP_PX: i32 = 2;
/// Largura de uma marca de limite, em px.
pub const MARK_PX: i32 = 2;
/// Diferenca de fracao abaixo da qual a regua nao muda: 1/1000 e menos de um
/// pixel numa regua de ate 1000 px.
pub const FILL_EPSILON: f64 = 1e-3;

/// Parte cheia de uma regua, em fracao da capacidade (0..=1 cada, soma <= 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    /// Fatia dos agentes (sessoes).
    pub agents: f64,
    /// Resto do uso: o uso total menos a fatia dos agentes.
    pub rest: f64,
    /// O uso passou da capacidade (atividade paralela acima dos nucleos): a
    /// regua enche e o texto diz o numero real.
    pub overflow: bool,
}

impl Fill {
    /// Uso total e fatia dos agentes sobre uma capacidade. `None` sem uso ou
    /// sem capacidade (positiva): a regua fica so com o trilho. Sem fatia dos
    /// agentes, o uso inteiro e resto. Agentes acima do uso (somas de
    /// amostras de instantes diferentes) ficam no uso: a fatia nunca passa do
    /// todo.
    pub fn of(used: Option<f64>, agents: Option<f64>, capacity: Option<f64>) -> Option<Fill> {
        let (used, cap) = (used?, capacity?);
        if cap.is_nan() || cap <= 0.0 || !used.is_finite() {
            return None;
        }
        let total = (used / cap).max(0.0);
        let shown = total.min(1.0);
        let agents = agents
            .filter(|a| a.is_finite())
            .map_or(0.0, |a| (a / cap).clamp(0.0, shown));
        Some(Fill {
            agents,
            rest: shown - agents,
            overflow: total > 1.0,
        })
    }

    /// Uma leitura so, sem divisao (pressao, contexto): tudo no segmento
    /// "resto", que nessas reguas tem a cor da leitura.
    pub fn single(value: Option<f64>, capacity: f64) -> Option<Fill> {
        Fill::of(value, None, Some(capacity))
    }

    /// Fracao total cheia (0..=1).
    pub fn total(&self) -> f64 {
        self.agents + self.rest
    }

    fn close_to(&self, o: &Fill) -> bool {
        (self.agents - o.agents).abs() < FILL_EPSILON
            && (self.rest - o.rest).abs() < FILL_EPSILON
            && self.overflow == o.overflow
    }
}

/// Estado desenhado de uma regua: o que mudou decide se realoca.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RuleState {
    pub fill: Option<Fill>,
    pub alert: bool,
}

impl RuleState {
    /// Troca o estado e diz se a regua precisa ser redesenhada. Diferenca
    /// abaixo de [`FILL_EPSILON`] conta como igual e nao troca nada.
    pub fn update(&mut self, fill: Option<Fill>, alert: bool) -> bool {
        let same_fill = match (&self.fill, &fill) {
            (None, None) => true,
            (Some(a), Some(b)) => a.close_to(b),
            _ => false,
        };
        if same_fill && self.alert == alert {
            return false;
        }
        self.fill = fill;
        self.alert = alert;
        true
    }
}

/// Larguras em px de cada segmento numa regua de `width` px: (agentes,
/// resto). Segmento que nao chega a 1 px some; com os dois presentes, a
/// folga sai do resto.
pub fn segment_widths(fill: &Fill, width: i32) -> (i32, i32) {
    let w = f64::from(width.max(0));
    let total = (fill.total() * w).round() as i32;
    let mut agents = (fill.agents * w).round() as i32;
    if fill.agents > 0.0 && agents == 0 && total > 0 {
        agents = 1;
    }
    let agents = agents.min(total);
    let mut rest = total - agents;
    if agents > 0 && rest > 0 {
        rest = (rest - SEGMENT_GAP_PX).max(0);
    }
    (agents, rest)
}

/// Posicao em px da marca de limite `at` (fracao), centrada e dentro da regua.
pub fn mark_x(at: f64, width: i32) -> i32 {
    let x = (at.clamp(0.0, 1.0) * f64::from(width)).round() as i32 - MARK_PX / 2;
    x.clamp(0, (width - MARK_PX).max(0))
}

// ---- Formatadores locais (format.rs e do C18) -------------------------------

const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// Divisor e unidade IEC em que `total` fica abaixo de 1024.
fn unit_of(total: u64) -> (f64, &'static str) {
    let mut div = 1.0;
    let mut u = 0;
    while total as f64 / div >= 1024.0 && u < UNITS.len() - 1 {
        div *= 1024.0;
        u += 1;
    }
    (div, UNITS[u])
}

/// `v` na unidade de `total`, com uma casa (bytes sem casa): `20.0 GiB`.
/// Abaixo de 0.1 dessa unidade (sairia `0.0 GiB`), na unidade do proprio
/// valor: `12 MiB`.
pub fn bytes_in_unit_of(v: u64, total: u64) -> String {
    let (div, unit) = unit_of(total);
    if div > 1.0 && (v as f64) < div * 0.1 {
        return crate::format::bytes(v as f64);
    }
    if div == 1.0 {
        format!("{v} {unit}")
    } else {
        format!("{:.1} {unit}", v as f64 / div)
    }
}

/// Leitura e capacidade separadas, na unidade da capacidade:
/// `("29.3", "of 38.0 GiB")`. A leitura vai em negrito, a capacidade em texto
/// secundario (DESIGN.md secao 2, "Type").
pub fn used_and_capacity(used: u64, total: u64) -> (String, String) {
    let (div, unit) = unit_of(total);
    if div == 1.0 {
        (format!("{used}"), format!("of {total} {unit}"))
    } else {
        (
            format!("{:.1}", used as f64 / div),
            format!("of {:.1} {unit}", total as f64 / div),
        )
    }
}

/// Percentual inteiro: `42%`.
pub fn pct0(v: f64) -> String {
    format!("{v:.0}%")
}

/// Percentual com uma casa: `12.5%`.
pub fn pct1(v: f64) -> String {
    format!("{v:.1}%")
}

// ---- Widget -----------------------------------------------------------------

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Rule {
        pub state: Cell<RuleState>,
        pub agents: RefCell<Option<gtk::Widget>>,
        pub rest: RefCell<Option<gtk::Widget>>,
        pub marks: RefCell<Vec<(f64, gtk::Widget)>>,
        pub value_text: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Rule {
        const NAME: &'static str = "AcRule";
        type Type = super::Rule;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("rule");
            // Leitor de tela: medidor com valor, minimo, maximo e texto.
            klass.set_accessible_role(gtk::AccessibleRole::Meter);
        }
    }

    impl ObjectImpl for Rule {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(false);
            let seg = |class: &str| {
                let w = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                w.add_css_class("segment");
                w.add_css_class(class);
                w.set_can_target(false);
                w.set_visible(false);
                w.set_parent(&*obj);
                w.upcast::<gtk::Widget>()
            };
            self.agents.replace(Some(seg("agents")));
            self.rest.replace(Some(seg("rest")));
        }

        fn dispose(&self) {
            for w in [self.agents.take(), self.rest.take()].into_iter().flatten() {
                w.unparent();
            }
            for (_, w) in self.marks.take() {
                w.unparent();
            }
        }
    }

    impl WidgetImpl for Rule {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // Altura e largura minimas vem do CSS (min-height/min-width do no
            // `rule`); aqui so a largura natural da regua grande, que a grade
            // estica. A pequena (`.mini`) fica no minimo do CSS.
            let natural = if self.obj().has_css_class("mini") {
                0
            } else {
                160
            };
            match orientation {
                gtk::Orientation::Horizontal => (0, natural, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let state = self.state.get();
            let (a, r) = state.fill.map_or((0, 0), |f| segment_widths(&f, width));
            let place = |w: &Option<gtk::Widget>, x: i32, len: i32| {
                let Some(w) = w else { return };
                w.set_visible(len > 0);
                if len > 0 {
                    w.size_allocate(&gtk::Allocation::new(x, 0, len, height), -1);
                }
            };
            place(&self.agents.borrow(), 0, a);
            let rest_x = if a > 0 { a + SEGMENT_GAP_PX } else { 0 };
            place(&self.rest.borrow(), rest_x, r);
            for (at, w) in self.marks.borrow().iter() {
                w.size_allocate(
                    &gtk::Allocation::new(mark_x(*at, width), 0, MARK_PX, height),
                    -1,
                );
            }
        }
    }
}

glib::wrapper! {
    /// Regua com trilho, dois segmentos e marcas de limite. CSS: `rule`
    /// (trilho), `rule > box.segment.agents`, `rule > box.segment.rest`,
    /// `rule > box.mark`; `rule.alert` quando a leitura passa do limite.
    pub struct Rule(ObjectSubclass<imp::Rule>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Rule {
    /// Regua com rotulo acessivel `label` e marcas de limite nas fracoes
    /// `marks` (por exemplo `[0.1]` para o limite de pressao).
    pub fn new(label: &str, marks: &[f64]) -> Self {
        let rule: Self = glib::Object::new();
        let imp = rule.imp();
        for at in marks {
            let m = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            m.add_css_class("mark");
            m.set_can_target(false);
            m.set_parent(&rule);
            imp.marks.borrow_mut().push((*at, m.upcast()));
        }
        rule.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::ValueMin(0.0),
            gtk::accessible::Property::ValueMax(100.0),
        ]);
        rule
    }

    /// Mostra `fill` (ou so o trilho, com `None`) e liga ou desliga o estado
    /// de alerta. Igual ao anterior: nada muda, nada repinta. Devolve se mudou.
    pub fn set(&self, fill: Option<Fill>, alert: bool) -> bool {
        let imp = self.imp();
        let mut state = imp.state.get();
        let was_alert = state.alert;
        if !state.update(fill, alert) {
            return false;
        }
        imp.state.set(state);
        if was_alert != alert {
            if alert {
                self.add_css_class("alert");
            } else {
                self.remove_css_class("alert");
            }
        }
        if fill.is_none() {
            self.add_css_class("absent");
        } else {
            self.remove_css_class("absent");
        }
        // Sem leitura nao ha valor: o leitor de tela fica com o ValueText
        // ("no reading"), nunca com um zero fabricado.
        match fill {
            Some(f) => self.update_property(&[gtk::accessible::Property::ValueNow(
                (f.total() * 100.0).round(),
            )]),
            None => self.reset_property(gtk::AccessibleProperty::ValueNow),
        }
        self.queue_allocate();
        true
    }

    /// Texto do valor para leitor de tela; so notifica a acessibilidade
    /// quando muda.
    pub fn set_value_text(&self, text: &str) {
        let imp = self.imp();
        if *imp.value_text.borrow() != text {
            self.update_property(&[gtk::accessible::Property::ValueText(text)]);
            imp.value_text.replace(text.to_owned());
        }
    }

    pub fn state(&self) -> RuleState {
        self.imp().state.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: f64 = (1u64 << 30) as f64;

    #[test]
    fn used_and_agents_split_the_capacity() {
        // tipico-40: RAM usada 29.3 GiB de 38.0, agentes 20.0 GiB.
        let f = Fill::of(Some(29.3 * GIB), Some(20.0 * GIB), Some(38.0 * GIB)).unwrap();
        assert!((f.total() - 29.3 / 38.0).abs() < 1e-9);
        assert!((f.agents - 20.0 / 38.0).abs() < 1e-9);
        assert!((f.rest - 9.3 / 38.0).abs() < 1e-9);
        assert!(!f.overflow);
    }

    #[test]
    fn absent_value_or_capacity_is_absent_not_zero() {
        assert_eq!(Fill::of(None, Some(1.0), Some(10.0)), None);
        assert_eq!(Fill::of(Some(1.0), None, None), None);
        assert_eq!(Fill::of(Some(1.0), None, Some(0.0)), None);
        assert_eq!(Fill::of(Some(f64::NAN), None, Some(1.0)), None);
        // Zero medido e uma medida: regua vazia, mas presente.
        let zero = Fill::of(Some(0.0), None, Some(10.0)).unwrap();
        assert_eq!(zero.total(), 0.0);
    }

    #[test]
    fn missing_agents_share_is_all_rest() {
        let f = Fill::of(Some(5.0), None, Some(10.0)).unwrap();
        assert_eq!((f.agents, f.rest), (0.0, 0.5));
    }

    #[test]
    fn agents_never_exceed_the_use_nor_the_capacity() {
        let f = Fill::of(Some(4.0), Some(6.0), Some(10.0)).unwrap();
        assert_eq!((f.agents, f.rest), (0.4, 0.0));
        let over = Fill::of(Some(80.0), Some(50.0), Some(16.0)).unwrap();
        assert!(over.overflow);
        assert_eq!(over.total(), 1.0);
        assert_eq!(over.agents, 1.0);
    }

    #[test]
    fn same_value_does_not_redraw() {
        let mut s = RuleState::default();
        let f = Fill::of(Some(5.0), Some(2.0), Some(10.0));
        assert!(s.update(f, false), "primeiro valor desenha");
        assert!(!s.update(f, false), "mesmo valor nao desenha");
        let nudge = Fill::of(Some(5.0 + 1e-4), Some(2.0), Some(10.0));
        assert!(!s.update(nudge, false), "menos de um pixel nao desenha");
        assert!(s.update(f, true), "alerta muda o desenho");
        assert!(s.update(None, true), "ficar sem valor muda o desenho");
        assert!(!s.update(None, true));
        assert!(s.update(Fill::of(Some(6.0), Some(2.0), Some(10.0)), true));
    }

    #[test]
    fn segments_keep_the_gap_and_fit_the_rule() {
        let f = Fill::of(Some(50.0), Some(25.0), Some(100.0)).unwrap();
        assert_eq!(segment_widths(&f, 400), (100, 98));
        // So agentes: sem folga.
        let a = Fill::of(Some(25.0), Some(25.0), Some(100.0)).unwrap();
        assert_eq!(segment_widths(&a, 400), (100, 0));
        // Fatia minima ainda aparece com 1 px.
        let tiny = Fill::of(Some(50.0), Some(0.01), Some(100.0)).unwrap();
        assert_eq!(segment_widths(&tiny, 400).0, 1);
        let empty = Fill::of(Some(0.0), None, Some(100.0)).unwrap();
        assert_eq!(segment_widths(&empty, 400), (0, 0));
        let full = Fill::of(Some(100.0), Some(40.0), Some(100.0)).unwrap();
        let (a, r) = segment_widths(&full, 300);
        assert_eq!(a + SEGMENT_GAP_PX + r, 300);
    }

    #[test]
    fn limit_mark_stays_inside_the_rule() {
        assert_eq!(mark_x(0.1, 200), 19);
        assert_eq!(mark_x(0.0, 200), 0);
        assert_eq!(mark_x(1.0, 200), 198);
        assert_eq!(mark_x(0.5, 0), 0);
    }

    #[test]
    fn readouts_use_the_unit_of_the_capacity() {
        let total = 40_802_189_312;
        let used = total - 9_341_553_868;
        assert_eq!(
            used_and_capacity(used, total),
            ("29.3".to_string(), "of 38.0 GiB".to_string())
        );
        assert_eq!(bytes_in_unit_of(21_484_273_664, total), "20.0 GiB");
        assert_eq!(
            used_and_capacity(300, 900),
            ("300".into(), "of 900 B".into())
        );
        assert_eq!(bytes_in_unit_of(12, 900), "12 B");
        // uma.json: 12.3 MiB de swap dos agentes em 27 GiB nao vira 0.0 GiB.
        assert_eq!(bytes_in_unit_of(12_919_135, 28_991_029_248), "12 MiB");
        assert_eq!(pct0(41.8), "42%");
        assert_eq!(pct1(12.5), "12.5%");
    }
}
