//! Compiles the eBPF program when the `netbpf` feature asks for it.
//!
//! clang is a build dependency only for that feature: the object is
//! embedded in the binary (`include_bytes!` in src/netbpf.rs), so nothing
//! compiles C on the machine it runs on. `-g` is not optional — the BTF it
//! emits is what lets the loader relocate the program's field reads
//! against whichever kernel it lands on.

fn main() {
    println!("cargo:rerun-if-changed=bpf/net.bpf.c");
    if std::env::var_os("CARGO_FEATURE_NETBPF").is_none() {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("net.bpf.o");
    let mut clang = std::process::Command::new(std::env::var("CLANG").unwrap_or_else(|_| "clang".into()));
    clang.args(["-target", "bpf", "-O2", "-g", "-Wall", "-Werror"]);
    // Debian and Ubuntu keep <asm/types.h> under a multiarch directory
    // that clang, targeting bpf rather than the host, doesn't search.
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let multiarch = format!("/usr/include/{arch}-linux-gnu");
    if std::path::Path::new(&multiarch).is_dir() {
        clang.arg(format!("-I{multiarch}"));
    }
    let status = clang.args(["-c", "bpf/net.bpf.c", "-o"]).arg(&out).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("clang couldn't compile bpf/net.bpf.c ({s})"),
        Err(e) => panic!("the netbpf feature needs clang (and libbpf's headers) to compile bpf/net.bpf.c: {e}"),
    }
}
