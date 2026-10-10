//! CPU time (not wall time) of each part of a recorder tick.
use hyprforge_procman::{cgroups, machine::SysFs, proc::ProcFs};
fn cpu_ms() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let us = |t: libc::timeval| t.tv_sec as f64 * 1e6 + t.tv_usec as f64;
    (us(u.ru_utime) + us(u.ru_stime)) / 1000.0
}
fn time(label: &str, mut f: impl FnMut()) {
    let c = cpu_ms();
    for _ in 0..10 { f(); }
    println!("{label:28} {:.1} ms CPU", (cpu_ms() - c) / 10.0);
}
fn main() {
    let mut fs = ProcFs::default();
    time("procs (stat/statm/cmdline)", || { std::hint::black_box(fs.snapshot().unwrap()); });
    fs.verbose = true;
    time("+ status", || { std::hint::black_box(fs.snapshot().unwrap()); });
    fs.threads = true;
    time("+ threads", || { std::hint::black_box(fs.snapshot().unwrap()); });
    let sys = SysFs::default();
    time("machine (no drive temps)", || { std::hint::black_box(sys.read()); });
    time("cgroups", || { std::hint::black_box(cgroups::all(std::path::Path::new("/sys/fs/cgroup"), 6)); });
}
