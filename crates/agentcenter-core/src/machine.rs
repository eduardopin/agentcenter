//! Maquina inteira: `/proc/meminfo`, `/proc/loadavg` e `/proc/pressure/*` (PSI).
//!
//! Tudo opcional: arquivo ausente ou fora do formato vira `None`, nunca erro de
//! tick. PSI so existe com `CONFIG_PSI=y` (e sem `psi=0` no boot).

use crate::procfs::{ProcRoot, kb_line_bytes};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Campos de `/proc/meminfo`, em bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub mem_total: Option<u64>,
    pub mem_available: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_free: Option<u64>,
}

pub fn parse_meminfo(text: &str) -> MemInfo {
    let mut m = MemInfo::default();
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "MemTotal" => &mut m.mem_total,
            "MemAvailable" => &mut m.mem_available,
            "SwapTotal" => &mut m.swap_total,
            "SwapFree" => &mut m.swap_free,
            _ => continue,
        };
        *slot = kb_line_bytes(val);
    }
    m
}

/// Media de carga de `/proc/loadavg` (1, 5 e 15 min).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoadAvg {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

pub fn parse_loadavg(text: &str) -> Option<LoadAvg> {
    let mut it = text.split_ascii_whitespace().map(|v| v.parse::<f64>().ok());
    Some(LoadAvg {
        one: it.next()??,
        five: it.next()??,
        fifteen: it.next()??,
    })
}

/// Um arquivo de `/proc/pressure/`: % do tempo em que alguma (`some`) ou todas
/// (`full`) as tarefas estavam paradas esperando o recurso, medias de 10 s e 60 s.
/// `full` falta em `cpu` em kernels anteriores ao 5.13.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PsiResource {
    pub some_avg10: Option<f64>,
    pub some_avg60: Option<f64>,
    pub full_avg10: Option<f64>,
    pub full_avg60: Option<f64>,
}

/// `None` se nenhuma linha `some`/`full` foi entendida.
pub fn parse_pressure(text: &str) -> Option<PsiResource> {
    let mut r = PsiResource::default();
    let mut seen = false;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let (avg10, avg60) = match fields.next() {
            Some("some") => (&mut r.some_avg10, &mut r.some_avg60),
            Some("full") => (&mut r.full_avg10, &mut r.full_avg60),
            _ => continue,
        };
        for f in fields {
            let Some((k, v)) = f.split_once('=') else {
                continue;
            };
            let slot = match k {
                "avg10" => &mut *avg10,
                "avg60" => &mut *avg60,
                _ => continue,
            };
            *slot = v.parse().ok();
            seen |= slot.is_some();
        }
    }
    seen.then_some(r)
}

/// PSI dos tres recursos. Em [`crate::Machine::psi`] e `None` quando o kernel
/// nao expoe `/proc/pressure`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Psi {
    pub cpu: Option<PsiResource>,
    pub memory: Option<PsiResource>,
    pub io: Option<PsiResource>,
}

/// O que a maquina inteira expoe neste tick, alem da linha `cpu` de `/proc/stat`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MachineReading {
    pub mem: MemInfo,
    pub load: Option<LoadAvg>,
    pub psi: Option<Psi>,
}

/// Le meminfo, loadavg e os tres arquivos de pressao. Cinco opens por tick.
pub fn read_machine(root: &ProcRoot, buf: &mut String) -> MachineReading {
    fn read<T>(root: &ProcRoot, rel: &str, buf: &mut String, f: fn(&str) -> T) -> Option<T> {
        root.read_into(Path::new(rel), buf).ok().map(|()| f(buf))
    }
    let mem = read(root, "meminfo", buf, parse_meminfo).unwrap_or_default();
    let load = read(root, "loadavg", buf, parse_loadavg).flatten();
    let psi = Psi {
        cpu: read(root, "pressure/cpu", buf, parse_pressure).flatten(),
        memory: read(root, "pressure/memory", buf, parse_pressure).flatten(),
        io: read(root, "pressure/io", buf, parse_pressure).flatten(),
    };
    let any = psi.cpu.is_some() || psi.memory.is_some() || psi.io.is_some();
    MachineReading {
        mem,
        load,
        psi: any.then_some(psi),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_picks_the_four_fields_in_bytes() {
        let m = parse_meminfo(
            "MemTotal:       65536 kB\nMemFree:  1 kB\nMemAvailable:   32768 kB\n\
             SwapCached: 9 kB\nSwapTotal:      8192 kB\nSwapFree:       4096 kB\n",
        );
        assert_eq!(m.mem_total, Some(65536 * 1024));
        assert_eq!(m.mem_available, Some(32768 * 1024));
        assert_eq!(m.swap_total, Some(8192 * 1024));
        assert_eq!(m.swap_free, Some(4096 * 1024));
        assert_eq!(parse_meminfo("MemTotal: x kB\n").mem_total, None);
    }

    #[test]
    fn loadavg_three_numbers_or_none() {
        let l = parse_loadavg("1.50 0.75 0.25 2/870 12345\n").unwrap();
        assert_eq!((l.one, l.five, l.fifteen), (1.5, 0.75, 0.25));
        assert_eq!(parse_loadavg("1.5 abc 0.2"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn pressure_some_and_full_with_optional_full() {
        let r = parse_pressure(
            "some avg10=1.25 avg60=0.50 avg300=0.10 total=123\n\
             full avg10=0.75 avg60=0.25 avg300=0.00 total=45\n",
        )
        .unwrap();
        assert_eq!(r.some_avg10, Some(1.25));
        assert_eq!(r.some_avg60, Some(0.5));
        assert_eq!(r.full_avg10, Some(0.75));
        assert_eq!(r.full_avg60, Some(0.25));
        // cpu em kernel antigo: so `some`.
        let cpu = parse_pressure("some avg10=2.00 avg60=1.00 avg300=0.00 total=9\n").unwrap();
        assert_eq!((cpu.some_avg10, cpu.full_avg10), (Some(2.0), None));
        assert_eq!(parse_pressure("garbage\n"), None);
    }
}
