//! Guard de rotulo: o modelo so pode devolver um rotulo da lista. O motivo da recusa e um token
//! curto e fixo; nunca cita a resposta do modelo.

use serde_json::Value;

/// Por que a saida foi recusada (`empty`, `not_json`, `missing_field`, `label_not_allowed`).
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub reason: String,
}

/// O que o guard decidiu.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Accepted(String),
    Refused(Refusal),
}

fn refused(reason: &str) -> Verdict {
    Verdict::Refused(Refusal {
        reason: reason.into(),
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Shape {
    /// A resposta inteira e o rotulo.
    Bare,
    /// A resposta e um objeto JSON e o rotulo esta no campo.
    Field(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Guard {
    shape: Shape,
    labels: Vec<String>,
}

impl Guard {
    /// A resposta, sem espacos nas pontas, e exatamente um dos rotulos.
    pub fn labels(labels: &[&str]) -> Guard {
        Guard {
            shape: Shape::Bare,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    /// A resposta e um objeto JSON com `field` string igual a um dos rotulos.
    pub fn json_label(field: &str, labels: &[&str]) -> Guard {
        Guard {
            shape: Shape::Field(field.into()),
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    pub fn check(&self, raw: &str) -> Verdict {
        let t = raw.trim();
        if t.is_empty() {
            return refused("empty");
        }
        let label = match &self.shape {
            Shape::Bare => t.to_string(),
            Shape::Field(f) => {
                let Ok(v) = serde_json::from_str::<Value>(t) else {
                    return refused("not_json");
                };
                match v.get(f).and_then(Value::as_str) {
                    Some(s) => s.to_string(),
                    None => return refused("missing_field"),
                }
            }
        };
        if self.labels.contains(&label) {
            Verdict::Accepted(label)
        } else {
            refused("label_not_allowed")
        }
    }
}
