//! `/proc/<pid>/task/*/stat`: fallback de stall quando `task_delayacct=0`.

use super::{ProcRoot, read_file_into};

/// Conta as threads do processo em estado `D` (sono nao interrompivel, quase
/// sempre E/S). Thread que some no meio da contagem e ignorada.
///
/// `main_state` e o estado do `/proc/<pid>/stat` ja lido no tick: e o da thread
/// principal (o kernel le o lider do grupo), entao `task/<pid>/stat` nao e relido.
pub fn count_d_threads(root: &ProcRoot, pid: u32, main_state: char, buf: &mut String) -> u32 {
    root.note_read(pid, "task");
    let Ok(dir) = std::fs::read_dir(root.pid_path(pid, "task")) else {
        return 0;
    };
    let mut n = u32::from(main_state == 'D');
    for entry in dir.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            == Some(pid)
        {
            continue;
        }
        if read_file_into(&entry.path().join("stat"), buf).is_err() {
            continue;
        }
        let state = buf
            .rfind(')')
            .and_then(|i| buf[i + 1..].split_ascii_whitespace().next());
        if state == Some("D") {
            n += 1;
        }
    }
    n
}
