//! Deteccao de processos agente. Cada agente e um arquivo com uma linha da tabela
//! ([`AdapterSpec`]); agente que precisa de regra propria implementa [`Adapter`].
//!
//! Agente que roda dentro de um interpretador (`comm` = `node`, `python3`...)
//! so e reconhecivel pelo argv: [`Registry::detect_with`] le a cmdline nesse caso
//! e pergunta a [`Adapter::matches_hosted`]. Ideia de olhar o "alvo" do
//! interpretador (script ou `-m modulo`) e nao a cmdline inteira, para que um
//! processo que so menciona o agente nos argumentos nao vire agente: agtop
//! (MIT, `src/matchers.rs` @ 7876b33, `match_head`).

pub mod aider;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor_agent;
pub mod gemini;
pub mod goose;
pub mod herdr;
pub mod kiro;
pub mod opencode;
pub mod pi;

use serde::{Deserialize, Serialize};

/// Variavel que acrescenta agentes por `comm`, separados por virgula.
pub const EXTRA_AGENTS_ENV: &str = "AGENTCENTER_EXTRA_AGENTS";

/// Papel do processo detectado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Agente de codigo (claude, codex, pi...).
    Agent,
    /// Multiplexador de terminais (herdr): medido como linha propria.
    Multiplexer,
}

/// Identifica um processo como agente. `exe` le o basename de `/proc/<pid>/exe`
/// sob demanda (custa uma syscall), entao so chame quando o `comm` nao bastar.
pub trait Adapter: Send + Sync {
    fn id(&self) -> &str;
    fn role(&self) -> AgentRole;
    fn matches(&self, comm: &str, exe: &mut dyn FnMut() -> Option<String>) -> bool;

    /// Reconhece o agente pelo argv quando o `comm` e de um interpretador.
    /// So chamado por [`Registry::detect_with`], depois que nenhum adapter casou
    /// por `comm`/`exe`. Padrao: nao casa.
    fn matches_hosted(&self, _cmd: &HostedCmd<'_>) -> bool {
        false
    }
}

/// Interpretador que hospeda agentes: o `comm` e dele, o agente esta no argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpreter {
    /// `node`, `nodejs`, `bun`.
    Node,
    /// `python`, `python3`, `python3.N`.
    Python,
}

/// Interpretador cujo `comm` e este, se for um. So nesses `comm` vale ler a
/// cmdline: os demais processos nunca custam essa leitura.
pub fn interpreter(comm: &str) -> Option<Interpreter> {
    match comm {
        "node" | "nodejs" | "bun" => Some(Interpreter::Node),
        "python" | "python3" => Some(Interpreter::Python),
        _ => comm
            .strip_prefix("python3.")
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .map(|_| Interpreter::Python),
    }
}

/// O que o interpretador executa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// Caminho do script (primeiro argumento que nao e opcao).
    Script(&'a str),
    /// `python -m <modulo>`.
    Module(&'a str),
}

/// Cmdline de um processo de interpretador, reduzida ao que identifica o agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostedCmd<'a> {
    pub interpreter: Interpreter,
    /// argv[0] como o processo o deixou (`exec -a` muda; nao e o executavel).
    pub argv0: &'a str,
    /// `None` em `node -e`, `python -c`, stdin ou sem argumentos.
    pub target: Option<Target<'a>>,
}

/// Opcoes do node que consomem o argumento seguinte.
const NODE_VALUE_FLAGS: &[&str] = &[
    "-r",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    "-C",
    "--conditions",
    "--env-file",
];
/// Opcoes do node cujo codigo vem na linha: nao ha script.
const NODE_EVAL_FLAGS: &[&str] = &["-e", "--eval", "-p", "--print"];
/// Opcoes do python que consomem o argumento seguinte.
const PYTHON_VALUE_FLAGS: &[&str] = &["-W", "-X"];

impl<'a> HostedCmd<'a> {
    /// `raw` e o conteudo de `/proc/<pid>/cmdline`: argumentos separados por NUL.
    pub fn parse(interpreter: Interpreter, raw: &'a str) -> Self {
        let mut args = raw.trim_end_matches('\0').split('\0');
        let argv0 = args.next().unwrap_or("");
        let target = match interpreter {
            Interpreter::Node => node_target(args),
            Interpreter::Python => python_target(args),
        };
        Self {
            interpreter,
            argv0,
            target,
        }
    }

    pub fn script(&self) -> Option<&'a str> {
        match self.target {
            Some(Target::Script(s)) => Some(s),
            _ => None,
        }
    }

    pub fn module(&self) -> Option<&'a str> {
        match self.target {
            Some(Target::Module(m)) => Some(m),
            _ => None,
        }
    }

    /// Basename do script sem `.js`/`.mjs`/`.cjs`/`.py`.
    pub fn script_name(&self) -> Option<&'a str> {
        let path = self.script()?;
        let base = path.rsplit('/').next().unwrap_or(path);
        Some(
            [".js", ".mjs", ".cjs", ".py"]
                .iter()
                .find_map(|ext| base.strip_suffix(ext))
                .unwrap_or(base),
        )
    }
}

fn node_target<'a>(mut args: impl Iterator<Item = &'a str>) -> Option<Target<'a>> {
    while let Some(a) = args.next() {
        if a == "--" {
            return args.next().map(Target::Script);
        }
        if NODE_EVAL_FLAGS.contains(&a) {
            return None;
        }
        if NODE_VALUE_FLAGS.contains(&a) {
            args.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return Some(Target::Script(a));
    }
    None
}

fn python_target<'a>(mut args: impl Iterator<Item = &'a str>) -> Option<Target<'a>> {
    while let Some(a) = args.next() {
        if a == "-m" {
            return args.next().map(Target::Module);
        }
        if let Some(m) = a.strip_prefix("-m") {
            return Some(Target::Module(m));
        }
        if a.starts_with("-c") || a == "-" {
            return None;
        }
        if PYTHON_VALUE_FLAGS.contains(&a) {
            args.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return Some(Target::Script(a));
    }
    None
}

/// Regras de [`Adapter::matches_hosted`] de uma linha da tabela.
#[derive(Debug)]
pub struct HostedRule {
    /// Nomes de script ([`HostedCmd::script_name`]).
    pub scripts: &'static [&'static str],
    /// Modulos de `python -m`: o proprio ou um submodulo (`aider` casa `aider.main`).
    pub modules: &'static [&'static str],
    /// Script cujo caminho contem o primeiro trecho e termina no segundo.
    pub paths: &'static [(&'static str, &'static str)],
}

impl HostedRule {
    /// Agente que nao roda dentro de interpretador.
    pub const NONE: HostedRule = HostedRule {
        scripts: &[],
        modules: &[],
        paths: &[],
    };

    fn matches(&self, cmd: &HostedCmd<'_>) -> bool {
        match cmd.target {
            Some(Target::Script(path)) => {
                cmd.script_name().is_some_and(|n| self.scripts.contains(&n))
                    || self
                        .paths
                        .iter()
                        .any(|(part, end)| path.contains(part) && path.ends_with(end))
            }
            Some(Target::Module(m)) => self.modules.iter().any(|x| {
                m.strip_prefix(x)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            }),
            None => false,
        }
    }
}

/// Linha da tabela de adapters.
#[derive(Debug)]
pub struct AdapterSpec {
    pub id: &'static str,
    pub role: AgentRole,
    /// `comm` exatos (o kernel trunca em 15 bytes).
    pub comms: &'static [&'static str],
    /// Basenames de executavel conferidos so quando o `comm` comeca com um deles
    /// (binario renomeado ou `comm` truncado), para nao ler `exe` de todo processo.
    pub exe_names: &'static [&'static str],
    /// Regras para quando o processo e um interpretador ([`Registry::detect_with`]).
    pub hosted: HostedRule,
}

impl Adapter for AdapterSpec {
    fn id(&self) -> &str {
        self.id
    }

    fn role(&self) -> AgentRole {
        self.role
    }

    fn matches(&self, comm: &str, exe: &mut dyn FnMut() -> Option<String>) -> bool {
        if self.comms.contains(&comm) {
            return true;
        }
        if self.exe_names.iter().any(|n| comm.starts_with(n)) {
            return exe().is_some_and(|e| self.exe_names.contains(&e.as_str()));
        }
        false
    }

    fn matches_hosted(&self, cmd: &HostedCmd<'_>) -> bool {
        self.hosted.matches(cmd)
    }
}

/// Agente acrescentado por [`EXTRA_AGENTS_ENV`]: id = comm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraAgent {
    pub comm: String,
}

impl Adapter for ExtraAgent {
    fn id(&self) -> &str {
        &self.comm
    }

    fn role(&self) -> AgentRole {
        AgentRole::Agent
    }

    fn matches(&self, comm: &str, _exe: &mut dyn FnMut() -> Option<String>) -> bool {
        comm == self.comm
    }
}

/// Adapters embutidos, na ordem em que sao consultados.
pub fn builtin() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(&claude::CLAUDE),
        Box::new(&codex::CODEX),
        Box::new(&pi::PI),
        Box::new(&herdr::HERDR),
        Box::new(&gemini::GEMINI),
        Box::new(&copilot::COPILOT),
        Box::new(&kiro::KIRO),
        Box::new(&opencode::OPENCODE),
        Box::new(&aider::AIDER),
        Box::new(&goose::GOOSE),
        Box::new(&cursor_agent::CURSOR_AGENT),
    ]
}

impl<T: Adapter + ?Sized> Adapter for &T {
    fn id(&self) -> &str {
        (**self).id()
    }
    fn role(&self) -> AgentRole {
        (**self).role()
    }
    fn matches(&self, comm: &str, exe: &mut dyn FnMut() -> Option<String>) -> bool {
        (**self).matches(comm, exe)
    }
    fn matches_hosted(&self, cmd: &HostedCmd<'_>) -> bool {
        (**self).matches_hosted(cmd)
    }
}

/// Le a lista de `comm` extras ("a, b,,c" -> [a, b, c]); vazios e repetidos saem.
pub fn parse_extra_agents(value: &str) -> Vec<ExtraAgent> {
    let mut out: Vec<ExtraAgent> = Vec::new();
    for comm in value.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        if !out.iter().any(|e| e.comm == comm) {
            out.push(ExtraAgent {
                comm: comm.to_string(),
            });
        }
    }
    out
}

/// Conjunto de adapters consultado pela arvore. O primeiro que casa decide.
pub struct Registry {
    adapters: Vec<Box<dyn Adapter>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            adapters: builtin(),
        }
    }
}

impl Registry {
    pub fn new(adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self { adapters }
    }

    /// Embutidos mais os extras de `value` (formato de [`EXTRA_AGENTS_ENV`]).
    /// Extra com o mesmo comm de um embutido e ignorado: o embutido vence.
    pub fn with_extra(value: &str) -> Self {
        let mut r = Self::default();
        for extra in parse_extra_agents(value) {
            let taken = r
                .adapters
                .iter()
                .any(|a| a.matches(&extra.comm, &mut || None));
            if !taken {
                r.adapters.push(Box::new(extra));
            }
        }
        r
    }

    /// Embutidos mais os extras da variavel de ambiente, se definida.
    pub fn from_env() -> Self {
        match std::env::var(EXTRA_AGENTS_ENV) {
            Ok(v) => Self::with_extra(&v),
            Err(_) => Self::default(),
        }
    }

    pub fn ids(&self) -> Vec<&str> {
        self.adapters.iter().map(|a| a.id()).collect()
    }

    /// Adapter que reconhece o processo, se houver.
    pub fn detect(
        &self,
        comm: &str,
        exe: &mut dyn FnMut() -> Option<String>,
    ) -> Option<&dyn Adapter> {
        self.adapters
            .iter()
            .find(|a| a.matches(comm, exe))
            .map(|a| a.as_ref())
    }

    /// Como [`Registry::detect`], e, se nenhum adapter casar e o `comm` for de
    /// um interpretador ([`interpreter`]), le a cmdline e consulta
    /// [`Adapter::matches_hosted`]. `cmdline` devolve o conteudo bruto de
    /// `/proc/<pid>/cmdline` (argumentos separados por NUL) e so e chamado
    /// nesse caso, como `exe`. Match por `comm`/`exe` vence o do argv.
    pub fn detect_with(
        &self,
        comm: &str,
        exe: &mut dyn FnMut() -> Option<String>,
        cmdline: &mut dyn FnMut() -> Option<String>,
    ) -> Option<&dyn Adapter> {
        if let Some(a) = self.detect(comm, exe) {
            return Some(a);
        }
        let interp = interpreter(comm)?;
        let raw = cmdline()?;
        let cmd = HostedCmd::parse(interp, &raw);
        self.adapters
            .iter()
            .find(|a| a.matches_hosted(&cmd))
            .map(|a| a.as_ref())
    }
}
