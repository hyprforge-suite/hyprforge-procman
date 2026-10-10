//! One process, looked at closely: its program, folder, unit, open files
//! and network sockets — and the reverse, which process has a port or a
//! file. `lsof` and `ss` in one place, where Windows splits them into
//! Resource Monitor.
//!
//! All of it is read from `/proc`, as the user: a process of their own
//! answers in full, another user's answers with what the kernel shows
//! anyone (its unit) and nothing else. Every function returns what it
//! could read and says nothing about what it could not; the window shows
//! "not yours to see" for the gaps rather than an empty list that looks
//! like an answer.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

/// The program file, if this user may see it.
pub fn exe(root: &Path, pid: i32) -> Option<PathBuf> {
    fs::read_link(root.join(pid.to_string()).join("exe")).ok()
}

/// The working folder, if this user may see it.
pub fn cwd(root: &Path, pid: i32) -> Option<PathBuf> {
    fs::read_link(root.join(pid.to_string()).join("cwd")).ok()
}

/// The systemd unit it runs in — the last `.service` or `.scope` in its
/// cgroup path — readable for every process.
pub fn unit(root: &Path, pid: i32) -> Option<String> {
    let cgroup = fs::read_to_string(root.join(pid.to_string()).join("cgroup")).ok()?;
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    path.rsplit('/').find(|part| part.ends_with(".service") || part.ends_with(".scope")).map(str::to_owned)
}

/// What each open descriptor points at, or `None` when they are not
/// this user's to read.
pub fn open(root: &Path, pid: i32) -> Option<Vec<String>> {
    let dir = fs::read_dir(root.join(pid.to_string()).join("fd")).ok()?;
    Some(dir.flatten().filter_map(|e| fs::read_link(e.path()).ok()).map(|p| p.to_string_lossy().into_owned()).collect())
}

fn socket_inode(target: &str) -> Option<u64> {
    target.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Proto {
    Tcp,
    Udp,
}

/// One socket, as `/proc/net/{tcp,udp}{,6}` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socket {
    pub proto: Proto,
    pub local: (IpAddr, u16),
    pub remote: (IpAddr, u16),
    /// `Listening`, `Established`, … — TCP's states in words.
    pub state: &'static str,
    pub inode: u64,
}

/// `0B00A8C0:C930`: the address is the kernel's in-memory bytes, so an
/// IPv4 one reads backwards (`0B00A8C0` is 192.168.0.11), and an IPv6
/// one is four such words; the port is plain big-endian hex.
fn endpoint(text: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match addr.len() {
        8 => IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(addr, 16).ok()?.swap_bytes())),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in addr.as_bytes().chunks(8).enumerate() {
                let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some((ip, port))
}

fn tcp_state(code: &str) -> &'static str {
    match code {
        "01" => "Established",
        "02" => "Opening",
        "03" => "Opening",
        "04" | "05" | "06" | "07" | "08" | "09" | "0B" => "Closing",
        "0A" => "Listening",
        _ => "Other",
    }
}

/// Every socket in one of the four tables.
pub fn parse_table(text: &str, proto: Proto) -> Vec<Socket> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let state = match proto {
                Proto::Tcp => tcp_state(f.get(3)?),
                // UDP's state column is 07 for a bound socket with no peer.
                Proto::Udp => {
                    if *f.get(3)? == "01" {
                        "Connected"
                    } else {
                        "Listening"
                    }
                }
            };
            Some(Socket { proto, local: endpoint(f.get(1)?)?, remote: endpoint(f.get(2)?)?, state, inode: f.get(9)?.parse().ok()? })
        })
        .collect()
}

/// The sockets in `pid`'s network namespace (its own `net/` directory, so
/// a process in a container answers with its container's table).
fn tables(root: &Path, pid: i32) -> Vec<Socket> {
    let net = root.join(pid.to_string()).join("net");
    let mut out = Vec::new();
    for (file, proto) in [("tcp", Proto::Tcp), ("tcp6", Proto::Tcp), ("udp", Proto::Udp), ("udp6", Proto::Udp)] {
        if let Ok(text) = fs::read_to_string(net.join(file)) {
            out.extend(parse_table(&text, proto));
        }
    }
    out
}

/// `pid`'s own sockets, or `None` when its descriptors are not this
/// user's to read.
pub fn sockets(root: &Path, pid: i32) -> Option<Vec<Socket>> {
    let inodes: HashSet<u64> = open(root, pid)?.iter().filter_map(|t| socket_inode(t)).collect();
    if inodes.is_empty() {
        return Some(Vec::new());
    }
    Some(tables(root, pid).into_iter().filter(|s| inodes.contains(&s.inode)).collect())
}

/// What a search can ask for besides a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// `port:8080` — whoever has a socket on that local or remote port.
    Port(u16),
    /// `file:~/notes.db` — whoever has that file (or anything under that
    /// folder) open.
    File(PathBuf),
}

impl Lookup {
    /// `port:N` or `file:PATH`, with `~` meaning home; anything else is
    /// an ordinary search.
    pub fn parse(query: &str, home: &Path) -> Option<Lookup> {
        let query = query.trim();
        if let Some(port) = query.strip_prefix("port:") {
            return port.trim().parse().ok().map(Lookup::Port);
        }
        let file = query.strip_prefix("file:")?.trim();
        if file.is_empty() {
            return None;
        }
        let path = match file.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(file),
        };
        Some(Lookup::File(path))
    }
}

/// The pids among `pids` that match `lookup` — only those whose
/// descriptors this user may read; the window says the others were not
/// looked at.
pub fn who(root: &Path, pids: &[i32], lookup: &Lookup) -> HashSet<i32> {
    let mut out = HashSet::new();
    match lookup {
        Lookup::Port(port) => {
            // One table read per namespace would be enough; one per
            // process is simpler and still only runs while such a search
            // is open.
            let mut by_ns: HashMap<u64, HashSet<u64>> = HashMap::new();
            for &pid in pids {
                let Some(targets) = open(root, pid) else { continue };
                let inodes: HashSet<u64> = targets.iter().filter_map(|t| socket_inode(t)).collect();
                if inodes.is_empty() {
                    continue;
                }
                let ns = fs::read_link(root.join(pid.to_string()).join("ns/net"))
                    .ok()
                    .and_then(|l| l.to_string_lossy().trim_start_matches("net:[").trim_end_matches(']').parse().ok())
                    .unwrap_or(0);
                let matching = by_ns.entry(ns).or_insert_with(|| {
                    tables(root, pid).into_iter().filter(|s| s.local.1 == *port || s.remote.1 == *port).map(|s| s.inode).collect()
                });
                if inodes.iter().any(|i| matching.contains(i)) {
                    out.insert(pid);
                }
            }
        }
        Lookup::File(path) => {
            for &pid in pids {
                let Some(targets) = open(root, pid) else { continue };
                if targets.iter().any(|t| Path::new(t).starts_with(path)) {
                    out.insert(pid);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines copied from this machine's `/proc/net/tcp` and `udp`.
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0B00A8C0:C930 9F8AB936:22B3 01 00000000:00000000 00:00000000 00000000  1000        0 365968 1 00000000192409de 35 4 30 2 2\n   1: 0100007F:8847 0100007F:E81E 01 00000000:00000000 00:00000000 00000000  1000        0 384121 1 000000000f8bad01 20 4 30 10 -1\n";
    const UDP: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n 5652: 0B00A8C0:0044 0100A8C0:0043 01 00000000:00000000 00:00000000 00000000     0        0 21769 2 00000000ef958ce4 0\n";

    #[test]
    fn a_socket_line_reads_its_backwards_address_and_plain_port() {
        let s = parse_table(TCP, Proto::Tcp);
        assert_eq!(s[0].local, ("192.168.0.11".parse().unwrap(), 51504));
        assert_eq!(s[0].remote, ("54.185.138.159".parse().unwrap(), 8883));
        assert_eq!((s[0].state, s[0].inode), ("Established", 365968));
        assert_eq!(s[1].local, ("127.0.0.1".parse().unwrap(), 34887));
        let u = parse_table(UDP, Proto::Udp);
        assert_eq!((u[0].local.1, u[0].remote.1, u[0].state), (68, 67, "Connected"), "DHCP");
    }

    #[test]
    fn an_ipv6_address_is_four_backwards_words() {
        // ::1 as the kernel prints it.
        assert_eq!(endpoint("00000000000000000000000001000000:0016"), Some(("::1".parse().unwrap(), 22)));
    }

    #[test]
    fn a_search_names_a_port_or_a_file_or_is_just_a_search() {
        let home = Path::new("/home/voldemort");
        assert_eq!(Lookup::parse("port:8080", home), Some(Lookup::Port(8080)));
        assert_eq!(Lookup::parse("file:~/horcruxes.db", home), Some(Lookup::File("/home/voldemort/horcruxes.db".into())));
        assert_eq!(Lookup::parse("firefox", home), None);
        assert_eq!(Lookup::parse("port:http", home), None);
    }

    #[test]
    fn a_cgroup_line_names_the_unit_it_runs_in() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("42")).unwrap();
        // This machine's own shell's /proc/self/cgroup.
        std::fs::write(t.path().join("42/cgroup"), "1:net_cls:/\n0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-ghostty-surface-transient-33493.scope\n").unwrap();
        assert_eq!(unit(t.path(), 42).as_deref(), Some("app-ghostty-surface-transient-33493.scope"));
    }

    /// The real thing: this test opens a file and a listening socket and
    /// finds itself by each.
    #[test]
    fn this_process_is_found_by_its_open_file_and_its_port() {
        let me = std::process::id() as i32;
        let root = Path::new("/proc");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("diary.txt");
        let _held = std::fs::File::create(&file).unwrap();
        assert!(who(root, &[me], &Lookup::File(file)).contains(&me));
        assert!(who(root, &[me], &Lookup::File(dir.path().into())).contains(&me), "a folder finds what is open inside it");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(who(root, &[me], &Lookup::Port(port)).contains(&me));
        let mine = sockets(root, me).unwrap();
        assert!(mine.iter().any(|s| s.local.1 == port && s.state == "Listening"));
        assert!(exe(root, me).is_some() && cwd(root, me).is_some());
    }
}
