//! Renderer padrao do GSK: `cairo`, salvo se o usuario definiu `GSK_RENDERER`.
//!
//! Medido nesta maquina com `scripts/measure-gui.sh` (2026-10-17, load 30-48,
//! 39 sessoes, 20 s): PSS de 29-43 MB no cairo, contra 113-125 MB no vulkan
//! e no gl (157-162 MB numa rodada anterior); CPU do cairo igual ou menor;
//! cold start 234-584 ms no cairo, 589-800 ms nos de GPU. So o cairo cabe no
//! orcamento de 80 MB. A janela e uma tabela de texto sem animacao: o que o
//! renderer de GPU acelera aqui nao paga o driver carregado no processo.

use std::ffi::OsStr;

pub const ENV: &str = "GSK_RENDERER";
pub const DEFAULT: &str = "cairo";

/// O valor a gravar em `GSK_RENDERER`, ou `None` para respeitar o do usuario.
/// Vazio conta como nao definido (o GTK tambem o ignora).
pub fn default_for(user: Option<&OsStr>) -> Option<&'static str> {
    match user {
        Some(v) if !v.is_empty() => None,
        _ => Some(DEFAULT),
    }
}

/// Aplica o padrao. Chamar no inicio do `main`, antes de qualquer thread.
pub fn apply_default() {
    if let Some(r) = default_for(std::env::var_os(ENV).as_deref()) {
        // SAFETY: roda no inicio do main, com o processo ainda de uma thread
        // so (o sampler e o GTK sobem depois), entao ninguem le o ambiente ao
        // mesmo tempo.
        unsafe { std::env::set_var(ENV, r) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_choice_wins() {
        for r in ["vulkan", "gl", "ngl", "cairo"] {
            assert_eq!(default_for(Some(OsStr::new(r))), None, "{r}");
        }
    }

    #[test]
    fn unset_or_empty_gets_cairo() {
        assert_eq!(default_for(None), Some("cairo"));
        assert_eq!(default_for(Some(OsStr::new(""))), Some("cairo"));
    }
}
