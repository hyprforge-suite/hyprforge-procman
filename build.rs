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
    let status = std::process::Command::new(std::env::var("CLANG").unwrap_or_else(|_| "clang".into()))
        .args(["-target", "bpf", "-O2", "-g", "-Wall", "-Werror", "-c", "bpf/net.bpf.c", "-o"])
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("clang couldn't compile bpf/net.bpf.c ({s})"),
        Err(e) => panic!("the netbpf feature needs clang (and libbpf's headers) to compile bpf/net.bpf.c: {e}"),
    }
}
