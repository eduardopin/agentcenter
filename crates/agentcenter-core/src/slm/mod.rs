//! Cliente de modelo local reutilizavel: `Endpoint` (so loopback), `Job` (prompt, gramatica,
//! tokens, prazo), `Guard` (confere a saida e diz por que recusou) e a marca `Source` em toda
//! saida apoiada em modelo. Sem endpoint nao ha conexao: `run` devolve o texto de regras.
//! Nenhum conteudo de transcricao passa por aqui; quem chama decide o que entra no prompt.

mod client;
mod endpoint;
mod guard;
mod job;

pub use client::{ClientError, complete, connection_attempts};
pub use endpoint::{Endpoint, EndpointError};
pub use guard::{Guard, Refusal, Verdict};
pub use job::{DEFAULT_MAX_TOKENS, Job, Message};

use serde::Serialize;

/// De onde veio o texto mostrado.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Rules,
    Model,
}

/// A saida de um job: o texto, de onde veio e, se o modelo foi tentado e nao serviu, por que.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome {
    pub text: String,
    pub source: Source,
    pub refused: Option<String>,
}

/// Roda `job` no `endpoint` e passa a resposta pelo `guard`; a saida e do modelo so se passar.
/// Sem endpoint, erro do cliente, estouro do prazo ou recusa: o texto de `rules`, com `source = rules`.
pub fn run(endpoint: Option<&Endpoint>, job: &Job, guard: &Guard, rules: &str) -> Outcome {
    let fallback = |refused: Option<String>| Outcome {
        text: rules.to_string(),
        source: Source::Rules,
        refused,
    };
    let Some(endpoint) = endpoint else {
        return fallback(None);
    };
    match complete(endpoint, job) {
        Err(e) => fallback(Some(e.to_string())),
        Ok(raw) => match guard.check(&raw) {
            Verdict::Accepted(text) => Outcome {
                text,
                source: Source::Model,
                refused: None,
            },
            Verdict::Refused(r) => fallback(Some(r.reason)),
        },
    }
}
