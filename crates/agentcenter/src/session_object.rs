//! Uma sessao no modelo da lista. O objeto vive enquanto a sessao (pid do
//! agente) existir: cada tick so troca as propriedades que mudaram, e a linha,
//! ligada por expressao, se redesenha sozinha.

use crate::format::Row;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::glib::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::SessionObject)]
    pub struct SessionObject {
        #[property(get, set)]
        pid: Cell<u32>,
        #[property(get, set)]
        agent: RefCell<String>,
        #[property(get, set)]
        name: RefCell<String>,
        #[property(get, set)]
        status: RefCell<String>,
        #[property(get, set)]
        group: Cell<u32>,
        #[property(get, set)]
        cpu: Cell<f64>,
        #[property(get, set)]
        cpu_text: RefCell<String>,
        #[property(get, set)]
        cpu_tooltip: RefCell<String>,
        #[property(get, set)]
        pss: Cell<i64>,
        #[property(get, set)]
        pss_text: RefCell<String>,
        #[property(get, set)]
        mem: Cell<i64>,
        #[property(get, set)]
        mem_text: RefCell<String>,
        #[property(get, set)]
        pss_pct: Cell<f64>,
        #[property(get, set)]
        pss_tooltip: RefCell<String>,
        #[property(get, set)]
        swap_pct: Cell<f64>,
        #[property(get, set)]
        swap_text: RefCell<String>,
        #[property(get, set)]
        swap_tooltip: RefCell<String>,
        #[property(get, set)]
        has_swap: Cell<bool>,
        #[property(get, set)]
        io_bound: Cell<bool>,
        #[property(get, set)]
        io_text: RefCell<String>,
        #[property(get, set)]
        io_tooltip: RefCell<String>,
        #[property(get, set)]
        ctx: Cell<f64>,
        #[property(get, set)]
        ctx_text: RefCell<String>,
        #[property(get, set)]
        ctx_tooltip: RefCell<String>,
        #[property(get, set)]
        place: RefCell<String>,
        #[property(get, set)]
        cwd: RefCell<String>,
        #[property(get, set)]
        meta: RefCell<String>,
        #[property(get, set)]
        rec: Cell<u32>,
        #[property(get, set)]
        rec_text: RefCell<String>,
        #[property(get, set)]
        rec_tooltip: RefCell<String>,
        #[property(get, set)]
        burn: Cell<f64>,
        #[property(get, set)]
        burn_text: RefCell<String>,
        #[property(get, set)]
        burn_tooltip: RefCell<String>,
        #[property(get, set)]
        burn_pct: Cell<f64>,
        #[property(get, set)]
        burn_pct_text: RefCell<String>,
        #[property(get, set)]
        cache_text: RefCell<String>,
        #[property(get, set)]
        cache_tooltip: RefCell<String>,
        #[property(get, set)]
        handoff_text: RefCell<String>,
        #[property(get, set)]
        handoff_tooltip: RefCell<String>,
        #[property(get, set)]
        tools_text: RefCell<String>,
        #[property(get, set)]
        tools_tooltip: RefCell<String>,
        #[property(get, set)]
        sub_text: RefCell<String>,
        #[property(get, set)]
        sub_tooltip: RefCell<String>,
        #[property(get, set)]
        a11y_label: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SessionObject {
        const NAME: &'static str = "AgentCenterSessionObject";
        type Type = super::SessionObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for SessionObject {}
}

glib::wrapper! {
    pub struct SessionObject(ObjectSubclass<imp::SessionObject>);
}

/// Propriedades do objeto, na ordem dos bits de [`Changed`] e das chamadas
/// de `sync!` em [`SessionObject::apply`] (conferido em build de debug).
pub const PROPS: [&str; 44] = [
    "pid",
    "agent",
    "name",
    "status",
    "group",
    "cpu",
    "cpu-text",
    "cpu-tooltip",
    "pss",
    "pss-text",
    "mem",
    "mem-text",
    "pss-pct",
    "pss-tooltip",
    "swap-pct",
    "swap-text",
    "swap-tooltip",
    "has-swap",
    "io-bound",
    "io-text",
    "io-tooltip",
    "ctx",
    "ctx-text",
    "ctx-tooltip",
    "place",
    "cwd",
    "meta",
    "rec",
    "rec-text",
    "rec-tooltip",
    "burn",
    "burn-text",
    "burn-tooltip",
    "burn-pct",
    "burn-pct-text",
    "cache-text",
    "cache-tooltip",
    "handoff-text",
    "handoff-tooltip",
    "tools-text",
    "tools-tooltip",
    "sub-text",
    "sub-tooltip",
    "a11y-label",
];

/// Conjunto de propriedades que mudaram num `apply`, um bit por [`PROPS`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changed(u64);

const _: () = assert!(PROPS.len() <= 64, "Changed has one bit per property");

impl Changed {
    /// So `prop`; vazio se o nome nao for uma propriedade.
    pub fn of(prop: &str) -> Self {
        PROPS
            .iter()
            .position(|p| *p == prop)
            .map_or(Self(0), |i| Self(1 << i))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn count(self) -> u32 {
        self.0.count_ones()
    }

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Changed {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Changed {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl SessionObject {
    pub fn new(row: &Row) -> Self {
        let obj: Self = glib::Object::new();
        obj.apply(row);
        obj
    }

    /// Troca so o que mudou e devolve o que mudou. Um `notify` por campo igual
    /// faria a linha refazer o layout a toa; o conjunto devolvido diz a lista
    /// se a chave da ordem mexeu (so entao a linha muda de lugar).
    pub fn apply(&self, row: &Row) -> Changed {
        let mut changed = Changed::default();
        let mut i = 0;
        // Notificacoes juntas: um so ciclo de atualizacao por objeto.
        let _freeze = self.freeze_notify();
        macro_rules! sync {
            ($get:ident, $set:ident, $v:expr) => {
                debug_assert_eq!(PROPS[i].replace('-', "_"), stringify!($get));
                if self.$get() != $v {
                    self.$set($v);
                    changed.0 |= 1 << i;
                }
                i += 1;
            };
        }
        sync!(pid, set_pid, row.pid);
        sync!(agent, set_agent, row.agent.as_str());
        sync!(name, set_name, row.name.as_str());
        sync!(status, set_status, row.status.as_str());
        sync!(group, set_group, row.group);
        sync!(cpu, set_cpu, row.cpu);
        sync!(cpu_text, set_cpu_text, row.cpu_text.as_str());
        sync!(cpu_tooltip, set_cpu_tooltip, row.cpu_tooltip.as_str());
        sync!(pss, set_pss, row.pss);
        sync!(pss_text, set_pss_text, row.pss_text.as_str());
        sync!(mem, set_mem, row.mem);
        sync!(mem_text, set_mem_text, row.mem_text.as_str());
        sync!(pss_pct, set_pss_pct, row.pss_pct);
        sync!(pss_tooltip, set_pss_tooltip, row.pss_tooltip.as_str());
        sync!(swap_pct, set_swap_pct, row.swap_pct);
        sync!(swap_text, set_swap_text, row.swap_text.as_str());
        sync!(swap_tooltip, set_swap_tooltip, row.swap_tooltip.as_str());
        sync!(has_swap, set_has_swap, row.has_swap);
        sync!(io_bound, set_io_bound, row.io_bound);
        sync!(io_text, set_io_text, row.io_text.as_str());
        sync!(io_tooltip, set_io_tooltip, row.io_tooltip.as_str());
        sync!(ctx, set_ctx, row.ctx);
        sync!(ctx_text, set_ctx_text, row.ctx_text.as_str());
        sync!(ctx_tooltip, set_ctx_tooltip, row.ctx_tooltip.as_str());
        sync!(place, set_place, row.place.as_str());
        sync!(cwd, set_cwd, row.cwd.as_str());
        sync!(meta, set_meta, row.meta.as_str());
        sync!(rec, set_rec, row.rec);
        sync!(rec_text, set_rec_text, row.rec_text.as_str());
        sync!(rec_tooltip, set_rec_tooltip, row.rec_tooltip.as_str());
        sync!(burn, set_burn, row.burn);
        sync!(burn_text, set_burn_text, row.burn_text.as_str());
        sync!(burn_tooltip, set_burn_tooltip, row.burn_tooltip.as_str());
        sync!(burn_pct, set_burn_pct, row.burn_pct);
        sync!(burn_pct_text, set_burn_pct_text, row.burn_pct_text.as_str());
        sync!(cache_text, set_cache_text, row.cache_text.as_str());
        sync!(cache_tooltip, set_cache_tooltip, row.cache_tooltip.as_str());
        sync!(handoff_text, set_handoff_text, row.handoff_text.as_str());
        sync!(
            handoff_tooltip,
            set_handoff_tooltip,
            row.handoff_tooltip.as_str()
        );
        sync!(tools_text, set_tools_text, row.tools_text.as_str());
        sync!(tools_tooltip, set_tools_tooltip, row.tools_tooltip.as_str());
        sync!(sub_text, set_sub_text, row.sub_text.as_str());
        sync!(sub_tooltip, set_sub_tooltip, row.sub_tooltip.as_str());
        sync!(a11y_label, set_a11y_label, row.a11y_label.as_str());
        debug_assert_eq!(i, PROPS.len());
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_prop_is_a_real_property() {
        let obj: SessionObject = glib::Object::new();
        for p in PROPS {
            assert!(obj.find_property(p).is_some(), "{p} is not a property");
        }
        assert!(Changed::of("nope").is_empty());
    }

    #[test]
    fn apply_reports_exactly_the_fields_that_moved() {
        let mut row = Row::default();
        let obj = SessionObject::new(&row);
        assert!(obj.apply(&row).is_empty(), "same row, nothing changed");
        row.cpu = 12.5;
        row.cpu_text = "12.5".into();
        let c = obj.apply(&row);
        assert_eq!(c, Changed::of("cpu") | Changed::of("cpu-text"));
        assert!(c.intersects(Changed::of("cpu")));
        assert!(!c.intersects(Changed::of("pss")));
        assert_eq!(c.count(), 2);
        row.a11y_label = "x".into();
        assert_eq!(
            obj.apply(&row),
            Changed::of("a11y-label"),
            "the last bit too"
        );
    }
}
