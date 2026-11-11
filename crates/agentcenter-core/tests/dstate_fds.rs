//! The task scan opens a directory fd and one fd per thread. Neither may leak
//! (the sampler runs it every tick for the life of the process) and neither may
//! survive an `exec` (the sampler also spawns children). Every test here
//! counts or inspects this process's fds, so they run one at a time.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::procfs::task::{
    count_d_threads, d_threads_besides_main, scan_d_threads_with,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// `(fd, link target)` of every fd this process holds.
fn open_fds() -> Vec<(u32, PathBuf)> {
    let mut v: Vec<_> = fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let fd: u32 = e.file_name().to_str()?.parse().ok()?;
            Some((fd, fs::read_link(e.path()).ok()?))
        })
        .collect();
    v.sort();
    v
}

/// `O_CLOEXEC` in the octal `flags:` line of `/proc/self/fdinfo/<fd>`.
fn fd_flags(fd: u32) -> u32 {
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
    let line = info.lines().find(|l| l.starts_with("flags:")).unwrap();
    u32::from_str_radix(line["flags:".len()..].trim(), 8).unwrap()
}

const O_CLOEXEC: u32 = 0o2000000;
const O_ACCMODE: u32 = 0o3;

#[test]
fn scans_do_not_leak_fds() {
    let _g = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new("ac-dfds-leak").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    for pid in 100..110 {
        let mut p = FakeProcess::new(pid, 1, "worker");
        p.threads = 20;
        p.d_threads = 5;
        fp.add(&p).unwrap();
    }
    // Paths that stop half way: a thread whose stat is a directory (read
    // fails after the open), a task that is not a directory, a dead pid.
    fs::remove_file(fp.root().join("100/task/101/stat")).ok();
    fs::create_dir_all(fp.root().join("100/task/101/stat")).unwrap();
    fs::remove_dir_all(fp.root().join("101/task")).unwrap();
    fs::write(fp.root().join("101/task"), "not a dir").unwrap();
    let root = fp.proc_root();
    let pids: Vec<u32> = (100..112).collect();
    let mut buf = String::new();
    // Warm up anything lazily opened (thread pools, buffers).
    let _ = scan_d_threads_with(&root, &pids, 3);
    let before = open_fds();
    for _ in 0..200 {
        for &pid in &pids {
            let _ = count_d_threads(&root, pid, 'S', &mut buf);
        }
        let _ = scan_d_threads_with(&root, &pids, 3);
    }
    let after = open_fds();
    assert_eq!(after, before, "fds held after the scans differ");
}

#[test]
fn scan_fds_are_close_on_exec() {
    let _g = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new("ac-dfds-cloexec").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let p = FakeProcess::new(100, 1, "worker");
    fp.add(&p).unwrap();
    let task = fp.root().join("100/task");
    let thread = task.join("200");
    fs::create_dir_all(&thread).unwrap();
    // A thread whose `stat` is a FIFO with a writer and no data: the scan
    // opens it and then waits in `read`, so its fds can be inspected.
    let fifo = thread.join("stat");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !matches!(made, Ok(s) if s.success()) {
        eprintln!("mkfifo is not available, skipping");
        return;
    }
    let writer = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let root = fp.proc_root();
    let scan = std::thread::spawn(move || {
        let mut buf = String::new();
        d_threads_besides_main(&root, 100, &mut buf)
    });
    let mut seen = None;
    for _ in 0..500 {
        let fds = open_fds();
        let task_fd = fds.iter().find(|(_, t)| *t == task).map(|(fd, _)| *fd);
        let stat_fd = fds
            .iter()
            .find(|(fd, t)| *t == fifo && fd_flags(*fd) & O_ACCMODE == 0)
            .map(|(fd, _)| *fd);
        if let (Some(t), Some(s)) = (task_fd, stat_fd) {
            seen = Some((fd_flags(t), fd_flags(s)));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Let the scan finish: a stat line makes the thread count as `D`.
    use std::io::Write;
    (&writer).write_all(b"200 (worker) D 1 0 0 0 0\n").unwrap();
    assert_eq!(scan.join().unwrap(), Some(1));
    let (task_flags, stat_flags) = seen.expect("the scan never held both fds at once");
    assert_ne!(task_flags & O_CLOEXEC, 0, "task fd flags {task_flags:o}");
    assert_ne!(stat_flags & O_CLOEXEC, 0, "stat fd flags {stat_flags:o}");
}
