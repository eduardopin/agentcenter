//! O que se pede ao modelo: prompt, gramatica opcional, teto de tokens e prazo total.

use serde::Serialize;
use std::time::Duration;

/// Uma mensagem do chat (`role` + `content`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Teto de tokens padrao (o do briefing).
pub const DEFAULT_MAX_TOKENS: u32 = 120;

/// Um pedido ao modelo. `timeout` cobre conexao, escrita e leitura juntas.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub prompt: Vec<Message>,
    /// Gramatica GBNF enviada ao servidor; o guard confere a saida de qualquer jeito.
    pub grammar: Option<String>,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl Job {
    pub fn new(prompt: Vec<Message>, timeout: Duration) -> Job {
        Job {
            prompt,
            grammar: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            timeout,
        }
    }
}
