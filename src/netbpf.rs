//! Network bytes per process, from the kernel: `bpf/net.bpf.c`, loaded and
//! read with aya.
//!
//! Root only (`CAP_BPF` and `CAP_PERFMON`). Whoever holds a [`Counter`]
//! — the recorder, the helper, `hyprforge-top` under sudo — fills each
//! process's [`NetIo`] from it before a sample is taken; everyone else
//! leaves it `None`, which every reader shows as "not measured".
//!
//! The counts run from the moment the program was loaded, by process id.
//! [`Counter::fill`] forgets the ids that are no longer running, so an id
//! the kernel hands to a new process starts again from nothing rather than
//! from the old one's total — except within the one interval in which the
//! old process ended and the new one began, which is the same window
//! `/proc`'s own counters are read through.

use crate::proc::{NetIo, Proc};
use aya::maps::{HashMap as BpfMap, MapData};
use aya::programs::BtfTracePoint;
use aya::{Btf, Ebpf};
use std::collections::{HashMap, HashSet};

/// The compiled program, embedded at build time.
static OBJECT: &[u8] = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/net.bpf.o"));

/// One process's counts as the program keeps them. Must match
/// `struct counts` in bpf/net.bpf.c.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    rx_bytes: u64,
    tx_bytes: u64,
    rx_calls: u64,
    tx_calls: u64,
}

// SAFETY: plain old data — four u64s, no padding, every bit pattern valid.
unsafe impl aya::Pod for Counts {}

/// The program, loaded and attached. Dropping it detaches both
/// tracepoints and frees the map.
pub struct Counter {
    /// Holds the programs and their links: they live as long as this.
    _ebpf: Ebpf,
    map: BpfMap<MapData, u32, Counts>,
}

impl std::fmt::Debug for Counter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Counter")
    }
}

impl Counter {
    /// Loads and attaches. Fails with the kernel's reason — most often
    /// "Operation not permitted", for anyone but root.
    pub fn start() -> Result<Counter, String> {
        let btf = Btf::from_sys_fs().map_err(|e| format!("this kernel has no BTF to load against: {e}"))?;
        let mut ebpf = aya::EbpfLoader::new().btf(Some(&btf)).load(OBJECT).map_err(|e| format!("couldn't load the network counter: {e}"))?;
        for (program, tracepoint) in [("send_length", "sock_send_length"), ("recv_length", "sock_recv_length")] {
            let p: &mut BtfTracePoint = ebpf
                .program_mut(program)
                .ok_or_else(|| format!("the network counter has no {program}"))?
                .try_into()
                .map_err(|e| format!("{program}: {e}"))?;
            p.load(tracepoint, &btf).map_err(|e| format!("couldn't load {program}: {e}"))?;
            p.attach().map_err(|e| format!("couldn't attach to {tracepoint}: {e}"))?;
        }
        let map = ebpf.take_map("NET_BY_PROCESS").ok_or("the network counter has no map")?;
        let map = BpfMap::try_from(map).map_err(|e| format!("the network counter's map: {e}"))?;
        Ok(Counter { _ebpf: ebpf, map })
    }

    /// Every process's counts by id, forgetting the ids not in
    /// `running` — so a reused id starts again from nothing.
    pub fn read(&mut self, running: &HashSet<u32>) -> HashMap<u32, NetIo> {
        let mut seen = HashMap::new();
        let mut gone = Vec::new();
        for (pid, c) in self.map.iter().flatten() {
            if running.contains(&pid) {
                seen.insert(pid, NetIo { rx_bytes: c.rx_bytes, tx_bytes: c.tx_bytes, rx_calls: c.rx_calls, tx_calls: c.tx_calls });
            } else {
                gone.push(pid);
            }
        }
        for pid in gone {
            // Already gone (the LRU, or a race with the exit) is fine.
            let _ = self.map.remove(&pid);
        }
        seen
    }

    /// Sets each process's [`NetIo`] — zero for one that has sent and
    /// received nothing since the counter started, which is a measurement
    /// rather than an absence — and forgets the ids no longer running.
    pub fn fill(&mut self, procs: &mut [Proc]) {
        let running: HashSet<u32> = procs.iter().map(|p| p.pid as u32).collect();
        let seen = self.read(&running);
        for p in procs.iter_mut() {
            p.net = Some(seen.get(&(p.pid as u32)).copied().unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_program_is_a_bpf_object_with_both_tracepoints() {
        // An ELF for machine 247 (EM_BPF), carrying the sections the
        // loader looks for — checked here, where no root is needed.
        assert_eq!(&OBJECT[..4], b"\x7fELF");
        assert_eq!(u16::from_le_bytes([OBJECT[18], OBJECT[19]]), 247);
        let text = String::from_utf8_lossy(OBJECT);
        for name in ["tp_btf/sock_send_length", "tp_btf/sock_recv_length", "NET_BY_PROCESS", "license"] {
            assert!(text.contains(name), "no {name} in the object");
        }
    }

    #[test]
    #[ignore = "loads eBPF: run as root"]
    fn as_root_a_download_is_counted_against_the_process_that_made_it() {
        // SAFETY: geteuid cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("HYPRFORGE-SKIP: loading eBPF needs root");
            return;
        }
        let mut counter = Counter::start().expect("loads as root");
        // Traffic of our own over loopback TCP: an Internet socket, so it
        // counts, and nothing leaves the machine.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            use std::io::Write;
            let (mut s, _) = server.accept().unwrap();
            s.write_all(&[7u8; 100_000]).unwrap();
        });
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut client, &mut got).unwrap();
        sender.join().unwrap();
        assert_eq!(got.len(), 100_000);

        let mut procs = vec![Proc { pid: std::process::id() as i32, ..Proc::default() }, Proc { pid: i32::MAX, ..Proc::default() }];
        counter.fill(&mut procs);
        let net = procs[0].net.expect("measured");
        // This process both sent and received the 100 000 bytes.
        assert!(net.rx_bytes >= 100_000 && net.tx_bytes >= 100_000, "{net:?}");
        assert_eq!(procs[1].net, Some(NetIo::default()), "a process that sent nothing is measured as nothing");
    }
}
