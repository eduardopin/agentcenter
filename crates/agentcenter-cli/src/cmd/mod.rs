//! Um modulo por subcomando.

pub mod cockpit;
pub mod reclaim;
pub mod signal;
pub mod snapshot;
pub mod watch;

/// Opcoes de linha de comando de `snapshot` e `watch`.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub interval_ms: u64,
    pub proc_root: Option<String>,
}

/// Opcoes comuns a `snapshot` e `watch`. `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String], cmd: &str, help: &str) -> Result<Option<Options>, String> {
    let mut o = Options {
        json: false,
        interval_ms: 1000,
        proc_root: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--json" => o.json = true,
            "--interval-ms" => {
                let v = value("--interval-ms")?;
                o.interval_ms = v
                    .parse()
                    .map_err(|_| format!("--interval-ms: `{v}` is not a number of milliseconds"))?;
            }
            "--proc-root" => o.proc_root = Some(value("--proc-root")?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("{cmd}: unknown option `{other}`\n\n{help}")),
        }
    }
    Ok(Some(o))
}
