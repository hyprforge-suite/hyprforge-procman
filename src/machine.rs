//! The machine as a whole: disks, networks, GPUs, sensors, pressure and
//! the battery — what the Performance tab draws beside CPU and memory.
//!
//! Read from `/proc` and `/sys` under a root that is `/` everywhere but
//! the tests, which lay out a directory the same way from files copied
//! off a real machine (a Framework 16: two NVMe drives, Wi-Fi, an AMD
//! APU beside an NVIDIA card, a ChromeOS embedded controller with two
//! fans).
//!
//! A sensor belongs to a device by **where its link points**, never by
//! its name: `hwmon*/device` and `block/<disk>/device` resolve to the
//! same directory for a drive's own thermometer, and a GPU's hwmon to
//! the card's PCI device. Two `nvme` sensors look the same by name and
//! are told apart only this way.
//!
//! Counters (bytes, sectors, busy milliseconds) are returned raw; rates
//! need two readings, and are [`rates`]' job.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Disk {
    /// `nvme1n1`.
    pub name: String,
    /// The drive's own name for itself, `WD_BLACK SN770 1TB`, if it has one.
    pub model: Option<String>,
    pub read_sectors: u64,
    pub write_sectors: u64,
    /// Milliseconds the device has had I/O in flight — "active time".
    pub busy_ms: u64,
    pub reads: u64,
    pub writes: u64,
    /// Milliseconds spent on reads and writes, summed over requests —
    /// over a count of requests, the average wait.
    pub read_ms: u64,
    pub write_ms: u64,
    pub in_flight: u64,
    /// Busy time weighted by how many were waiting: over elapsed time,
    /// the average queue.
    pub queue_ms: u64,
    pub celsius: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Net {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    /// Errors and drops, both ways, since the interface came up.
    pub errors: u64,
    pub drops: u64,
    /// `/sys/class/net/<name>/wireless` exists.
    pub wireless: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Gpu {
    /// `card2`.
    pub card: String,
    /// The kernel driver, `amdgpu` or `nvidia`.
    pub driver: String,
    /// The driver's own busy figure, when it gives one (amdgpu does).
    pub busy_percent: Option<u8>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub celsius: Option<f32>,
    /// Package power in watts, from the driver's sensor — on an APU this
    /// is the CPU and GPU together (amdgpu's `PPT`).
    pub watts: Option<f32>,
    /// Runtime power management: `active`, or `suspended` when a laptop's
    /// second GPU is asleep to save the battery. Read without waking it.
    pub power: Option<String>,
    /// The PCI address, `0000:01:00.0`.
    pub pci: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fan {
    pub label: String,
    pub rpm: u32,
}

/// Pressure stall information: the share of the last ten seconds in
/// which something was *waiting* for the resource. `some`: at least one
/// task waited; `full`: everything runnable did. Busy is not the same as
/// slow; this is the figure that says slow.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Pressure {
    pub cpu_some: f32,
    pub memory_some: f32,
    pub memory_full: f32,
    pub io_some: f32,
    pub io_full: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Battery {
    pub status: String,
    pub percent: u8,
    /// What it is drawing or taking, when the battery says.
    pub watts: Option<f32>,
}

/// One pressure line's three windows: the last 10 s, 60 s and 5 min.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Windows {
    pub avg10: f32,
    pub avg60: f32,
    pub avg300: f32,
}

/// Every pressure line, with all three windows, for the Advanced view.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PressureDetail {
    pub cpu_some: Windows,
    pub memory_some: Windows,
    pub memory_full: Windows,
    pub io_some: Windows,
    pub io_full: Windows,
}

/// Where memory went, in bytes, from `/proc/meminfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MemoryDetail {
    pub free: u64,
    /// Programs' own memory, not backed by any file.
    pub anon: u64,
    pub buffers: u64,
    pub cached: u64,
    pub shmem: u64,
    pub slab: u64,
    /// Of the slab, what the kernel can give back.
    pub slab_reclaimable: u64,
    pub page_tables: u64,
    pub kernel_stack: u64,
    /// Written to, not yet on disk; being written out.
    pub dirty: u64,
    pub writeback: u64,
    pub swap_cached: u64,
    /// Memory promised to programs, and the most the kernel will promise.
    pub committed: u64,
    pub commit_limit: u64,
    /// zswap's compressed pool, and what it holds uncompressed.
    pub zswap: u64,
    pub zswapped: u64,
    /// Transparent huge pages in programs' memory; reserved huge pages.
    pub anon_huge: u64,
    pub huge_total: u64,
    pub huge_free: u64,
}

/// Paging totals since boot, from `/proc/vmstat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Paging {
    pub faults: u64,
    pub major_faults: u64,
    /// Pages swapped in and out.
    pub swap_in: u64,
    pub swap_out: u64,
    /// Pages looked at, and taken back, to free memory — by kswapd and
    /// by programs that had to stop and reclaim it themselves.
    pub scanned: u64,
    pub stolen: u64,
    /// Times a program stalled to free memory, or to compact it.
    pub alloc_stalls: u64,
    pub compact_stalls: u64,
    pub oom_kills: u64,
}

/// One mounted filesystem, by device: btrfs mounts one device at several
/// places (`/`, `/home`, …), and it is one filesystem, listed once.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Filesystem {
    pub device: String,
    /// Where it is mounted — the shortest of its mount points.
    pub mount: String,
    pub kind: String,
    pub total: u64,
    /// What an ordinary user can still write.
    pub available: u64,
}

/// The network stack's totals since boot, from `/proc/net/snmp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Transport {
    pub ip_in: u64,
    pub ip_out: u64,
    pub ip_forwarded: u64,
    pub ip_delivered: u64,
    pub tcp_in: u64,
    pub tcp_out: u64,
    pub tcp_retransmitted: u64,
    pub tcp_active_opens: u64,
    pub tcp_passive_opens: u64,
    pub tcp_in_errors: u64,
    pub tcp_resets_sent: u64,
    pub tcp_established: u64,
    pub udp_in: u64,
    pub udp_out: u64,
    pub udp_in_errors: u64,
    pub udp_no_port: u64,
}

/// `/proc/net/snmp` is pairs of lines: `Tcp: Name Name …` then
/// `Tcp: 1 2 …`. Read by name, never by position, because the kernel
/// adds columns.
pub fn parse_snmp(text: &str) -> Transport {
    let mut values: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let lines: Vec<&str> = text.lines().collect();
    for pair in lines.chunks(2) {
        let [names, numbers] = pair else { continue };
        let (Some((group, names)), Some((_, numbers))) = (names.split_once(':'), numbers.split_once(':')) else { continue };
        for (name, number) in names.split_whitespace().zip(numbers.split_whitespace()) {
            if let Ok(n) = number.parse::<i64>() {
                values.insert(format!("{group}.{name}"), n.max(0) as u64);
            }
        }
    }
    let v = |k: &str| values.get(k).copied().unwrap_or(0);
    Transport {
        ip_in: v("Ip.InReceives"),
        ip_out: v("Ip.OutRequests"),
        ip_forwarded: v("Ip.ForwDatagrams"),
        ip_delivered: v("Ip.InDelivers"),
        tcp_in: v("Tcp.InSegs"),
        tcp_out: v("Tcp.OutSegs"),
        tcp_retransmitted: v("Tcp.RetransSegs"),
        tcp_active_opens: v("Tcp.ActiveOpens"),
        tcp_passive_opens: v("Tcp.PassiveOpens"),
        tcp_in_errors: v("Tcp.InErrs"),
        tcp_resets_sent: v("Tcp.OutRsts"),
        tcp_established: v("Tcp.CurrEstab"),
        udp_in: v("Udp.InDatagrams"),
        udp_out: v("Udp.OutDatagrams"),
        udp_in_errors: v("Udp.InErrors"),
        udp_no_port: v("Udp.NoPorts"),
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Machine {
    pub disks: Vec<Disk>,
    pub nets: Vec<Net>,
    pub gpus: Vec<Gpu>,
    /// The CPU's own temperature (`k10temp`'s Tctl, `coretemp`'s package).
    pub cpu_celsius: Option<f32>,
    pub fans: Vec<Fan>,
    pub pressure: Option<Pressure>,
    pub battery: Option<Battery>,
    /// Load average over 1, 5 and 15 minutes.
    pub load: Option<[f32; 3]>,
    pub uptime_seconds: Option<u64>,
    /// Each core's clock now, in MHz, and its most.
    pub clocks: Vec<(Option<u32>, Option<u32>)>,
    pub memory: MemoryDetail,
    pub paging: Paging,
    pub filesystems: Vec<Filesystem>,
    pub pressure_detail: Option<PressureDetail>,
    pub transport: Transport,
}

/// `/`, or a directory laid out like it.
#[derive(Debug, Clone)]
pub struct SysFs {
    root: PathBuf,
    /// Read each drive's temperature. Off by default, because it is the
    /// one sensor here that costs: an NVMe drive's thermometer is read by
    /// sending the drive a command — 30 to 80 ms each on this machine,
    /// and a drive asked every few seconds may never reach its deepest
    /// power-saving state. Callers ask now and then, not every reading.
    pub drive_temperatures: bool,
}

impl Default for SysFs {
    fn default() -> SysFs {
        SysFs { root: PathBuf::from("/"), drive_temperatures: false }
    }
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn number<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read(path)?.parse().ok()
}

/// One hwmon chip: its name, the device it belongs to, and its directory.
struct Chip {
    name: String,
    device: Option<PathBuf>,
    dir: PathBuf,
}

impl Chip {
    /// The first temperature, or the one labelled `label`.
    fn celsius(&self, label: Option<&str>) -> Option<f32> {
        for i in 1..=16 {
            let input = self.dir.join(format!("temp{i}_input"));
            let Some(milli) = number::<i64>(&input) else { continue };
            let this = read(self.dir.join(format!("temp{i}_label")));
            if label.is_none() || this.as_deref() == label {
                return Some(milli as f32 / 1000.0);
            }
        }
        None
    }
}

/// The chip whose `device` link resolves to `device`.
fn chip_on<'a>(chips: &'a [Chip], device: Option<&Path>) -> Option<&'a Chip> {
    let device = device?;
    chips.iter().find(|c| c.device.as_deref() == Some(device))
}

impl SysFs {
    pub fn rooted_at(root: impl Into<PathBuf>) -> SysFs {
        SysFs { root: root.into(), drive_temperatures: true }
    }

    fn path(&self, rest: &str) -> PathBuf {
        self.root.join(rest)
    }

    fn chips(&self) -> Vec<Chip> {
        let Ok(dir) = fs::read_dir(self.path("sys/class/hwmon")) else { return Vec::new() };
        let mut chips: Vec<Chip> = dir
            .flatten()
            .filter_map(|e| {
                let dir = e.path();
                Some(Chip { name: read(dir.join("name"))?, device: fs::canonicalize(dir.join("device")).ok(), dir })
            })
            .collect();
        chips.sort_by(|a, b| a.dir.cmp(&b.dir));
        chips
    }

    pub fn read(&self) -> Machine {
        let chips = self.chips();
        let cpu_celsius = chips
            .iter()
            .find(|c| c.name == "k10temp")
            .and_then(|c| c.celsius(Some("Tctl")))
            .or_else(|| chips.iter().find(|c| c.name == "coretemp").and_then(|c| c.celsius(Some("Package id 0"))));
        let fans = chips
            .iter()
            .flat_map(|c| {
                (1..=8).filter_map(move |i| {
                    let rpm = number::<u32>(c.dir.join(format!("fan{i}_input")))?;
                    let label = read(c.dir.join(format!("fan{i}_label"))).unwrap_or_else(|| format!("Fan {i}"));
                    Some(Fan { label, rpm })
                })
            })
            .collect();

        Machine {
            disks: self.disks(&chips),
            nets: self.nets(),
            gpus: self.gpus(&chips),
            cpu_celsius,
            fans,
            pressure: self.pressure(),
            battery: self.battery(),
            load: read(self.path("proc/loadavg")).and_then(|l| {
                let f: Vec<f32> = l.split_whitespace().take(3).filter_map(|v| v.parse().ok()).collect();
                Some([*f.first()?, *f.get(1)?, *f.get(2)?])
            }),
            uptime_seconds: read(self.path("proc/uptime")).and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok()).map(|s| s as u64),
            clocks: self.clocks(),
            memory: self.memory_detail(),
            paging: self.paging(),
            filesystems: self.filesystems(),
            pressure_detail: self.pressure_detail(),
            transport: parse_snmp(&read(self.path("proc/net/snmp")).unwrap_or_default()),
        }
    }

    fn disks(&self, chips: &[Chip]) -> Vec<Disk> {
        let stats = read(self.path("proc/diskstats")).unwrap_or_default();
        let mut disks = Vec::new();
        for line in stats.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let Some(&name) = f.get(2) else { continue };
            // Whole drives only: a partition has no `device` link of its
            // own under /sys/block, and loop, ram and zram are not drives.
            let block = self.path(&format!("sys/block/{name}"));
            if !block.join("device").exists() || ["loop", "ram", "zram"].iter().any(|p| name.starts_with(p)) {
                continue;
            }
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let device = fs::canonicalize(block.join("device")).ok();
            disks.push(Disk {
                name: name.to_owned(),
                model: read(block.join("device/model")).filter(|m| !m.is_empty()),
                read_sectors: n(5),
                write_sectors: n(9),
                busy_ms: n(12),
                reads: n(3),
                writes: n(7),
                read_ms: n(6),
                write_ms: n(10),
                in_flight: n(11),
                queue_ms: n(13),
                celsius: if self.drive_temperatures {
                    chip_on(chips, device.as_deref()).and_then(|c| c.celsius(Some("Composite")).or_else(|| c.celsius(None)))
                } else {
                    None
                },
            });
        }
        disks
    }

    fn nets(&self) -> Vec<Net> {
        let dev = read(self.path("proc/net/dev")).unwrap_or_default();
        dev.lines()
            .skip(2)
            .filter_map(|line| {
                let (name, rest) = line.split_once(':')?;
                let name = name.trim();
                if name == "lo" {
                    return None;
                }
                let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                Some(Net {
                    name: name.to_owned(),
                    rx_bytes: *f.first()?,
                    tx_bytes: *f.get(8)?,
                    rx_packets: *f.get(1)?,
                    tx_packets: *f.get(9)?,
                    errors: f.get(2)? + f.get(10)?,
                    drops: f.get(3)? + f.get(11)?,
                    wireless: self.path(&format!("sys/class/net/{name}/wireless")).exists(),
                })
            })
            .collect()
    }

    fn gpus(&self, chips: &[Chip]) -> Vec<Gpu> {
        let Ok(dir) = fs::read_dir(self.path("sys/class/drm")) else { return Vec::new() };
        let mut cards: Vec<String> = dir
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit()))
            .collect();
        cards.sort();
        cards
            .into_iter()
            .filter_map(|card| {
                let device = self.path(&format!("sys/class/drm/{card}/device"));
                let driver = fs::read_link(device.join("driver")).ok()?.file_name()?.to_string_lossy().into_owned();
                let resolved = fs::canonicalize(&device).ok();
                let chip = chip_on(chips, resolved.as_deref());
                Some(Gpu {
                    power: read(device.join("power/runtime_status")),
                    pci: resolved.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    busy_percent: number(device.join("gpu_busy_percent")),
                    vram_used: number(device.join("mem_info_vram_used")),
                    vram_total: number(device.join("mem_info_vram_total")),
                    celsius: chip.and_then(|c| c.celsius(Some("edge")).or_else(|| c.celsius(None))),
                    watts: chip.and_then(|c| {
                        number::<u64>(c.dir.join("power1_average")).or_else(|| number(c.dir.join("power1_input"))).map(|uw| uw as f32 / 1e6)
                    }),
                    card,
                    driver,
                })
            })
            .collect()
    }

    fn pressure(&self) -> Option<Pressure> {
        let avg10 = |file: &str, kind: &str| -> Option<f32> {
            let text = read(self.path(&format!("proc/pressure/{file}")))?;
            let line = text.lines().find(|l| l.starts_with(kind))?;
            line.split_whitespace().find_map(|w| w.strip_prefix("avg10=")).and_then(|v| v.parse().ok())
        };
        Some(Pressure {
            cpu_some: avg10("cpu", "some")?,
            memory_some: avg10("memory", "some")?,
            memory_full: avg10("memory", "full")?,
            io_some: avg10("io", "some")?,
            io_full: avg10("io", "full")?,
        })
    }

    fn clocks(&self) -> Vec<(Option<u32>, Option<u32>)> {
        let mut out = Vec::new();
        for i in 0.. {
            let dir = self.path(&format!("sys/devices/system/cpu/cpu{i}"));
            if !dir.exists() {
                break;
            }
            let khz = |f: &str| number::<u32>(dir.join("cpufreq").join(f)).map(|k| k / 1000);
            out.push((khz("scaling_cur_freq"), khz("cpuinfo_max_freq")));
        }
        out
    }

    fn memory_detail(&self) -> MemoryDetail {
        let text = read(self.path("proc/meminfo")).unwrap_or_default();
        let kib = |key: &str| {
            text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':')?.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0) * 1024
        };
        MemoryDetail {
            free: kib("MemFree"),
            anon: kib("AnonPages"),
            buffers: kib("Buffers"),
            cached: kib("Cached"),
            shmem: kib("Shmem"),
            slab: kib("Slab"),
            slab_reclaimable: kib("SReclaimable"),
            page_tables: kib("PageTables"),
            kernel_stack: kib("KernelStack"),
            dirty: kib("Dirty"),
            writeback: kib("Writeback"),
            swap_cached: kib("SwapCached"),
            committed: kib("Committed_AS"),
            commit_limit: kib("CommitLimit"),
            zswap: kib("Zswap"),
            zswapped: kib("Zswapped"),
            anon_huge: kib("AnonHugePages"),
            // A count of pages, not KiB: times the page size.
            huge_total: kib("HugePages_Total") / 1024 * kib("Hugepagesize"),
            huge_free: kib("HugePages_Free") / 1024 * kib("Hugepagesize"),
        }
    }

    fn paging(&self) -> Paging {
        let text = read(self.path("proc/vmstat")).unwrap_or_default();
        let n = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')?.trim().parse().ok()).unwrap_or(0);
        let has = |key: &str| text.lines().any(|l| l.split_once(' ').is_some_and(|(k, _)| k == key));
        // Reclaim is counted twice over on current kernels: once by who
        // reclaimed (kswapd, direct, khugepaged, proactive) and once by
        // what (anon, file) — each split is the whole, so adding every
        // `pgscan_*` line counts it twice (and `pgscan_direct_throttle`
        // is a different thing again). The by-what pair where the kernel
        // has it; the by-who lines on older kernels, which do not.
        let reclaim = |what: &str| {
            if has(&format!("{what}_anon")) {
                n(&format!("{what}_anon")) + n(&format!("{what}_file"))
            } else {
                n(&format!("{what}_kswapd")) + n(&format!("{what}_direct"))
            }
        };
        // Allocation stalls are split by zone, and zones are disjoint.
        let stalls: u64 = ["dma", "dma32", "normal", "movable", "device"].iter().map(|z| n(&format!("allocstall_{z}"))).sum();
        Paging {
            faults: n("pgfault"),
            major_faults: n("pgmajfault"),
            swap_in: n("pswpin"),
            swap_out: n("pswpout"),
            scanned: reclaim("pgscan"),
            stolen: reclaim("pgsteal"),
            alloc_stalls: stalls,
            compact_stalls: n("compact_stall"),
            oom_kills: n("oom_kill"),
        }
    }

    /// Real filesystems only — those on a block device — once each.
    fn filesystems(&self) -> Vec<Filesystem> {
        let mounts = read(self.path("proc/mounts")).unwrap_or_default();
        let mut by_device: std::collections::BTreeMap<String, (String, String)> = std::collections::BTreeMap::new();
        for line in mounts.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (Some(&device), Some(&mount), Some(&kind)) = (f.first(), f.get(1), f.get(2)) else { continue };
            if !device.starts_with("/dev/") {
                continue;
            }
            // `\040` is a space in a mount point.
            let mount = mount.replace("\\040", " ");
            let keep = by_device.get(device).is_none_or(|(m, _)| mount.len() < m.len());
            if keep {
                by_device.insert(device.to_owned(), (mount, kind.to_owned()));
            }
        }
        by_device
            .into_iter()
            .filter_map(|(device, (mount, kind))| {
                let (total, available) = space(&self.root.join(mount.trim_start_matches('/')))?;
                Some(Filesystem { device, mount, kind, total, available })
            })
            .collect()
    }

    fn pressure_detail(&self) -> Option<PressureDetail> {
        let windows = |file: &str, kind: &str| -> Option<Windows> {
            let text = read(self.path(&format!("proc/pressure/{file}")))?;
            let line = text.lines().find(|l| l.starts_with(kind))?;
            let get = |key: &str| line.split_whitespace().find_map(|w| w.strip_prefix(key)).and_then(|v| v.parse().ok());
            Some(Windows { avg10: get("avg10=")?, avg60: get("avg60=")?, avg300: get("avg300=")? })
        };
        Some(PressureDetail {
            cpu_some: windows("cpu", "some")?,
            memory_some: windows("memory", "some")?,
            memory_full: windows("memory", "full")?,
            io_some: windows("io", "some")?,
            io_full: windows("io", "full")?,
        })
    }

    fn battery(&self) -> Option<Battery> {
        let dir = fs::read_dir(self.path("sys/class/power_supply")).ok()?;
        let bat = dir.flatten().map(|e| e.path()).find(|p| read(p.join("type")).as_deref() == Some("Battery"))?;
        let watts = number::<u64>(bat.join("power_now")).map(|uw| uw as f32 / 1e6).or_else(|| {
            let ua = number::<u64>(bat.join("current_now"))?;
            let uv = number::<u64>(bat.join("voltage_now"))?;
            Some((ua as f64 * uv as f64 / 1e12) as f32)
        });
        Some(Battery { status: read(bat.join("status"))?, percent: number(bat.join("capacity"))?, watts })
    }
}

/// `statvfs`: total size, and what an ordinary user may still write.
fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter; `c` is a valid path.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let block = s.f_frsize as u64;
    Some((s.f_blocks as u64 * block, s.f_bavail as u64 * block))
}

/// What two readings say: bytes a second, and active time.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskRate {
    pub read: f64,
    pub write: f64,
    /// 0 to 1: the share of the time the drive had work in flight.
    pub active: f32,
    /// Requests a second.
    pub read_ops: f64,
    pub write_ops: f64,
    /// How long a request took, on average, in milliseconds — what a
    /// slow disk feels like, where throughput only says how much moved.
    pub wait_ms: Option<f64>,
    /// How many requests were waiting, on average.
    pub queue: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NetRate {
    pub rx: f64,
    pub tx: f64,
    pub rx_packets: f64,
    pub tx_packets: f64,
    /// Errors and drops in this interval — anything above 0 is worth
    /// seeing.
    pub errors: u64,
    pub drops: u64,
}

/// Each disk's or network's rate, by name.
pub type Named<T> = Vec<(String, T)>;

/// Rates for every disk and network in both readings, by name.
pub fn rates(before: &Machine, now: &Machine, seconds: f64) -> (Named<DiskRate>, Named<NetRate>) {
    if seconds <= 0.0 {
        return (Vec::new(), Vec::new());
    }
    let disks = now
        .disks
        .iter()
        .filter_map(|d| {
            let b = before.disks.iter().find(|b| b.name == d.name)?;
            // The kernel counts 512-byte sectors whatever the drive's own.
            Some((
                d.name.clone(),
                {
                    let ops = d.reads.saturating_sub(b.reads) + d.writes.saturating_sub(b.writes);
                    let ms = d.read_ms.saturating_sub(b.read_ms) + d.write_ms.saturating_sub(b.write_ms);
                    DiskRate {
                        read: d.read_sectors.saturating_sub(b.read_sectors) as f64 * 512.0 / seconds,
                        write: d.write_sectors.saturating_sub(b.write_sectors) as f64 * 512.0 / seconds,
                        active: (d.busy_ms.saturating_sub(b.busy_ms) as f64 / (seconds * 1000.0)).min(1.0) as f32,
                        read_ops: d.reads.saturating_sub(b.reads) as f64 / seconds,
                        write_ops: d.writes.saturating_sub(b.writes) as f64 / seconds,
                        wait_ms: (ops > 0).then(|| ms as f64 / ops as f64),
                        queue: d.queue_ms.saturating_sub(b.queue_ms) as f64 / (seconds * 1000.0),
                    }
                },
            ))
        })
        .collect();
    let nets = now
        .nets
        .iter()
        .filter_map(|n| {
            let b = before.nets.iter().find(|b| b.name == n.name)?;
            Some((
                n.name.clone(),
                NetRate {
                    rx: n.rx_bytes.saturating_sub(b.rx_bytes) as f64 / seconds,
                    tx: n.tx_bytes.saturating_sub(b.tx_bytes) as f64 / seconds,
                    rx_packets: n.rx_packets.saturating_sub(b.rx_packets) as f64 / seconds,
                    tx_packets: n.tx_packets.saturating_sub(b.tx_packets) as f64 / seconds,
                    errors: n.errors.saturating_sub(b.errors),
                    drops: n.drops.saturating_sub(b.drops),
                },
            ))
        })
        .collect();
    (disks, nets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A Framework 16's `/sys` and `/proc`, as far as this reads them —
    /// values copied from the real machine on 2026-10-10.
    fn framework() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let w = |p: &str, s: &str| {
            let p = root.join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        let dir = |p: &str| fs::create_dir_all(root.join(p)).unwrap();
        let link = |target: &str, at: &str| {
            let at = root.join(at);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            symlink(root.join(target), at).unwrap();
        };
        // Two drives, a partition and zram.
        w("proc/diskstats", " 259       0 nvme1n1 760853 88492 57309250 230119 3429423 1211 229359969 12037849 0 232081 12273352 4799 0 42384088 4031 4998 1352\n 259       1 nvme1n1p1 692 1645 46266 176 3149 770 499633 1424 0 144 1601 0 0 0 0 0 0\n 259       3 nvme0n1 438 0 22560 145 0 0 0 0 0 111 145 0 0 0 0 0 0\n 252       0 zram0 100 0 800 0 50 0 400 0 0 10 10 0 0 0 0 0 0\n");
        let wd = "sys/devices/pci0000:00/0000:00:02.4/0000:04:00.0/nvme/nvme1";
        let micron = "sys/devices/pci0000:00/0000:00:01.2/0000:02:00.0/nvme/nvme0";
        w(&format!("{wd}/model"), "WD_BLACK SN770 1TB                      \n");
        w(&format!("{micron}/model"), "2550 Micron 512GB                       \n");
        link(wd, "sys/block/nvme1n1/device");
        link(micron, "sys/block/nvme0n1/device");
        dir("sys/block/zram0");
        // Sensors: each drive's, the APU's, the CPU's, the fans.
        w("sys/class/hwmon/hwmon5/name", "nvme\n");
        w("sys/class/hwmon/hwmon5/temp1_input", "36850\n");
        w("sys/class/hwmon/hwmon5/temp1_label", "Composite\n");
        w("sys/class/hwmon/hwmon5/temp2_input", "46850\n");
        w("sys/class/hwmon/hwmon5/temp2_label", "Sensor 1\n");
        link(wd, "sys/class/hwmon/hwmon5/device");
        w("sys/class/hwmon/hwmon4/name", "nvme\n");
        w("sys/class/hwmon/hwmon4/temp1_input", "34850\n");
        w("sys/class/hwmon/hwmon4/temp1_label", "Composite\n");
        link(micron, "sys/class/hwmon/hwmon4/device");
        let apu = "sys/devices/pci0000:00/0000:00:08.1/0000:c3:00.0";
        w(&format!("{apu}/gpu_busy_percent"), "5\n");
        w(&format!("{apu}/mem_info_vram_used"), "1860530176\n");
        w(&format!("{apu}/mem_info_vram_total"), "2147483648\n");
        dir("sys/bus/pci/drivers/amdgpu");
        link("sys/bus/pci/drivers/amdgpu", &format!("{apu}/driver"));
        link(apu, "sys/class/drm/card2/device");
        dir("sys/class/drm/card2-DP-2");
        w("sys/class/hwmon/hwmon7/name", "amdgpu\n");
        w("sys/class/hwmon/hwmon7/temp1_input", "52000\n");
        w("sys/class/hwmon/hwmon7/temp1_label", "edge\n");
        w("sys/class/hwmon/hwmon7/power1_average", "12414000\n");
        link(apu, "sys/class/hwmon/hwmon7/device");
        // The NVIDIA card: a driver link and a power state, nothing else.
        let dgpu = "sys/devices/pci0000:00/0000:00:01.1/0000:01:00.0";
        w(&format!("{dgpu}/power/runtime_status"), "active\n");
        dir("sys/bus/pci/drivers/nvidia");
        link("sys/bus/pci/drivers/nvidia", &format!("{dgpu}/driver"));
        link(dgpu, "sys/class/drm/card1/device");
        w("sys/class/hwmon/hwmon8/name", "k10temp\n");
        w("sys/class/hwmon/hwmon8/temp1_input", "60250\n");
        w("sys/class/hwmon/hwmon8/temp1_label", "Tctl\n");
        w("sys/class/hwmon/hwmon9/name", "cros_ec\n");
        w("sys/class/hwmon/hwmon9/fan1_input", "1797\n");
        w("sys/class/hwmon/hwmon9/fan2_input", "1807\n");
        // Network, pressure, battery.
        w("proc/net/dev", "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 1189770    7159    0    0    0     0          0         0  1189770    7159    0    0    0     0       0          0\n wlan0: 27977946370 25346701    0    0    0     0          0         0 444592519 1678119    0    0    0     0       0          0\n");
        dir("sys/class/net/wlan0/wireless");
        w("proc/pressure/cpu", "some avg10=0.14 avg60=1.90 avg300=0.89 total=126077701\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
        w("proc/pressure/memory", "some avg10=0.00 avg60=0.00 avg300=0.00 total=2263304\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=2178070\n");
        w("proc/pressure/io", "some avg10=87.44 avg60=87.96 avg300=85.98 total=58473548516\nfull avg10=83.36 avg60=82.59 avg300=79.08 total=55678948269\n");
        w("sys/class/power_supply/ACAD/type", "Mains\n");
        w("sys/class/power_supply/BAT1/type", "Battery\n");
        w("sys/class/power_supply/BAT1/status", "Not charging\n");
        w("sys/class/power_supply/BAT1/capacity", "90\n");
        w("sys/class/power_supply/BAT1/current_now", "0\n");
        w("sys/class/power_supply/BAT1/voltage_now", "17204000\n");
        w("proc/loadavg", "2.21 2.16 2.54 2/1124 1239420\n");
        w("proc/uptime", "66567.25 688206.95\n");
        w("proc/vmstat", "pgpgin 29853745\npgpgout 120545912\npswpin 1132\npswpout 358547\npgfault 705063422\npgmajfault 76805\npgsteal_kswapd 8\npgsteal_direct 1\npgsteal_khugepaged 0\npgsteal_proactive 0\npgsteal_anon 2\npgsteal_file 7\npgscan_kswapd 10\npgscan_direct 2\npgscan_khugepaged 0\npgscan_proactive 0\npgscan_anon 3\npgscan_file 9\nallocstall_dma 0\nallocstall_dma32 1\nallocstall_normal 2\nallocstall_movable 0\nallocstall_device 0\npgscan_direct_throttle 50\ncompact_stall 4\noom_kill 0\n");
        t
    }

    #[test]
    fn drives_are_whole_disks_with_their_own_names_and_thermometers() {
        let t = framework();
        let m = SysFs::rooted_at(t.path()).read();
        let names: Vec<&str> = m.disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["nvme1n1", "nvme0n1"], "no partition, no zram");
        let wd = &m.disks[0];
        assert_eq!(wd.model.as_deref(), Some("WD_BLACK SN770 1TB"));
        assert_eq!((wd.read_sectors, wd.write_sectors, wd.busy_ms), (57309250, 229359969, 232081));
        // Each drive gets its own sensor, though both are called "nvme".
        assert_eq!(wd.celsius, Some(36.85));
        assert_eq!(m.disks[1].celsius, Some(34.85));
    }

    #[test]
    fn the_apu_reads_busy_vram_temperature_and_package_power() {
        let t = framework();
        let m = SysFs::rooted_at(t.path()).read();
        assert_eq!(m.gpus.len(), 2, "card2-DP-2 is a connector, not a card; the NVIDIA card is listed though it says little");
        let nvidia = m.gpus.iter().find(|g| g.driver == "nvidia").unwrap();
        assert_eq!((nvidia.busy_percent, nvidia.power.as_deref(), nvidia.pci.as_str()), (None, Some("active"), "0000:01:00.0"));
        let m = Machine { gpus: m.gpus.into_iter().filter(|g| g.driver == "amdgpu").collect(), ..m };
        let g = &m.gpus[0];
        assert_eq!((g.driver.as_str(), g.busy_percent, g.vram_total), ("amdgpu", Some(5), Some(2147483648)));
        assert_eq!(g.celsius, Some(52.0));
        assert!((g.watts.unwrap() - 12.414).abs() < 1e-3);
    }

    #[test]
    fn the_cpu_fans_network_pressure_and_battery_read_as_themselves() {
        let t = framework();
        let m = SysFs::rooted_at(t.path()).read();
        assert_eq!(m.cpu_celsius, Some(60.25));
        assert_eq!(m.fans.iter().map(|f| f.rpm).collect::<Vec<_>>(), [1797, 1807]);
        assert_eq!(m.nets, [Net { name: "wlan0".into(), rx_bytes: 27977946370, tx_bytes: 444592519, rx_packets: 25346701, tx_packets: 1678119, errors: 0, drops: 0, wireless: true }]);
        let p = m.pressure.unwrap();
        assert_eq!((p.io_some, p.io_full, p.cpu_some), (87.44, 83.36, 0.14));
        let d = m.pressure_detail.unwrap();
        assert_eq!((d.io_some.avg60, d.io_some.avg300), (87.96, 85.98));
        assert_eq!(m.load, Some([2.21, 2.16, 2.54]));
        assert_eq!(m.uptime_seconds, Some(66567));
        assert_eq!(m.paging, Paging { faults: 705063422, major_faults: 76805, swap_in: 1132, swap_out: 358547, scanned: 12, stolen: 9, alloc_stalls: 3, compact_stalls: 4, oom_kills: 0 });
        assert_eq!(m.battery, Some(Battery { status: "Not charging".into(), percent: 90, watts: Some(0.0) }));
    }

    /// This machine's `/proc/net/snmp` (the Ip, Tcp and Udp pairs).
    #[test]
    fn snmp_is_read_by_column_name() {
        let t = parse_snmp("Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates OutTransmits\nIp: 2 64 98267 0 1 0 0 0 98264 11109 28 0 0 0 0 0 0 0 0 11109\nTcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\nTcp: 1 200 120000 -1 150 68 2 37 35 8307 15796 21 0 48 0\nUdp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\nUdp: 89500 143 0 4013 0 0 0 245 0\n");
        assert_eq!((t.ip_in, t.ip_delivered, t.ip_out), (98267, 98264, 11109));
        assert_eq!((t.tcp_in, t.tcp_out, t.tcp_retransmitted, t.tcp_established, t.tcp_resets_sent), (8307, 15796, 21, 35, 48));
        assert_eq!((t.udp_in, t.udp_out, t.udp_no_port), (89500, 4013, 143));
    }

    #[test]
    fn rates_come_from_two_readings_in_bytes_and_active_time() {
        let mut a = Machine::default();
        a.disks.push(Disk {
            name: "d".into(),
            model: None,
            read_sectors: 0,
            write_sectors: 0,
            busy_ms: 0,
            reads: 0,
            writes: 0,
            read_ms: 0,
            write_ms: 0,
            in_flight: 0,
            queue_ms: 0,
            celsius: None,
        });
        a.nets.push(Net { name: "n".into(), rx_bytes: 0, tx_bytes: 0, rx_packets: 0, tx_packets: 0, errors: 0, drops: 0, wireless: false });
        let mut b = a.clone();
        b.disks[0].read_sectors = 2048;
        b.disks[0].busy_ms = 500;
        b.disks[0].reads = 40;
        b.disks[0].read_ms = 200;
        b.disks[0].queue_ms = 1500;
        b.nets[0].rx_bytes = 3000;
        b.nets[0].drops = 2;
        let (disks, nets) = rates(&a, &b, 1.0);
        let d = disks[0].1;
        assert_eq!((d.read, d.write, d.active), (1048576.0, 0.0, 0.5));
        assert_eq!((d.read_ops, d.wait_ms, d.queue), (40.0, Some(5.0), 1.5), "200 ms over 40 requests; 1.5 waiting on average");
        assert_eq!((nets[0].1.rx, nets[0].1.drops), (3000.0, 2));
    }

    /// And the real machine reads without failing, whatever it has.
    /// btrfs mounts one device at /, /home, /var/log…: one filesystem.
    #[test]
    fn a_device_mounted_several_times_is_one_filesystem_at_its_shortest_mount() {
        let t = tempfile::tempdir().unwrap();
        // This machine's /proc/mounts lines, trimmed.
        std::fs::create_dir_all(t.path().join("proc")).unwrap();
        std::fs::create_dir_all(t.path().join("home")).unwrap();
        std::fs::write(
            t.path().join("proc/mounts"),
            "/dev/nvme1n1p2 /home btrfs rw,relatime 0 0\n/dev/nvme1n1p2 / btrfs rw,relatime 0 0\nproc /proc proc rw 0 0\n",
        )
        .unwrap();
        let fs = SysFs::rooted_at(t.path()).read().filesystems;
        assert_eq!(fs.len(), 1);
        assert_eq!((fs[0].device.as_str(), fs[0].mount.as_str(), fs[0].kind.as_str()), ("/dev/nvme1n1p2", "/", "btrfs"));
        assert!(fs[0].total > 0);
    }

    #[test]
    fn the_real_machine_reads() {
        let m = SysFs::default().read();
        assert!(m.pressure.is_some() || !Path::new("/proc/pressure").exists());
        assert!(!m.filesystems.is_empty() && !m.clocks.is_empty() && m.load.is_some());
    }
}
