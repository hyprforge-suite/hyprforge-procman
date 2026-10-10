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

#[derive(Debug, Clone, PartialEq)]
pub struct Disk {
    /// `nvme1n1`.
    pub name: String,
    /// The drive's own name for itself, `WD_BLACK SN770 1TB`, if it has one.
    pub model: Option<String>,
    pub read_sectors: u64,
    pub write_sectors: u64,
    /// Milliseconds the device has had I/O in flight — "active time".
    pub busy_ms: u64,
    pub celsius: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Net {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// `/sys/class/net/<name>/wireless` exists.
    pub wireless: bool,
}

#[derive(Debug, Clone, PartialEq)]
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fan {
    pub label: String,
    pub rpm: u32,
}

/// Pressure stall information: the share of the last ten seconds in
/// which something was *waiting* for the resource. `some`: at least one
/// task waited; `full`: everything runnable did. Busy is not the same as
/// slow; this is the figure that says slow.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pressure {
    pub cpu_some: f32,
    pub memory_some: f32,
    pub memory_full: f32,
    pub io_some: f32,
    pub io_full: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub status: String,
    pub percent: u8,
    /// What it is drawing or taking, when the battery says.
    pub watts: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Machine {
    pub disks: Vec<Disk>,
    pub nets: Vec<Net>,
    pub gpus: Vec<Gpu>,
    /// The CPU's own temperature (`k10temp`'s Tctl, `coretemp`'s package).
    pub cpu_celsius: Option<f32>,
    pub fans: Vec<Fan>,
    pub pressure: Option<Pressure>,
    pub battery: Option<Battery>,
}

/// `/`, or a directory laid out like it.
#[derive(Debug, Clone)]
pub struct SysFs {
    root: PathBuf,
}

impl Default for SysFs {
    fn default() -> SysFs {
        SysFs { root: PathBuf::from("/") }
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
        SysFs { root: root.into() }
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
                celsius: chip_on(chips, device.as_deref()).and_then(|c| c.celsius(Some("Composite")).or_else(|| c.celsius(None))),
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

/// What two readings say: bytes a second, and active time.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DiskRate {
    pub read: f64,
    pub write: f64,
    /// 0 to 1: the share of the time the drive had work in flight.
    pub active: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NetRate {
    pub rx: f64,
    pub tx: f64,
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
                DiskRate {
                    read: d.read_sectors.saturating_sub(b.read_sectors) as f64 * 512.0 / seconds,
                    write: d.write_sectors.saturating_sub(b.write_sectors) as f64 * 512.0 / seconds,
                    active: (d.busy_ms.saturating_sub(b.busy_ms) as f64 / (seconds * 1000.0)).min(1.0) as f32,
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
        assert_eq!(m.gpus.len(), 1, "card2-DP-2 is a connector, not a card");
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
        assert_eq!(m.nets, [Net { name: "wlan0".into(), rx_bytes: 27977946370, tx_bytes: 444592519, wireless: true }]);
        let p = m.pressure.unwrap();
        assert_eq!((p.io_some, p.io_full, p.cpu_some), (87.44, 83.36, 0.14));
        assert_eq!(m.battery, Some(Battery { status: "Not charging".into(), percent: 90, watts: Some(0.0) }));
    }

    #[test]
    fn rates_come_from_two_readings_in_bytes_and_active_time() {
        let mut a = Machine::default();
        a.disks.push(Disk { name: "d".into(), model: None, read_sectors: 0, write_sectors: 0, busy_ms: 0, celsius: None });
        a.nets.push(Net { name: "n".into(), rx_bytes: 0, tx_bytes: 0, wireless: false });
        let mut b = a.clone();
        b.disks[0].read_sectors = 2048;
        b.disks[0].busy_ms = 500;
        b.nets[0].rx_bytes = 3000;
        let (disks, nets) = rates(&a, &b, 1.0);
        assert_eq!(disks[0].1, DiskRate { read: 1048576.0, write: 0.0, active: 0.5 });
        assert_eq!(nets[0].1, NetRate { rx: 3000.0, tx: 0.0 });
    }

    /// And the real machine reads without failing, whatever it has.
    #[test]
    fn the_real_machine_reads() {
        let m = SysFs::default().read();
        assert!(m.pressure.is_some() || !Path::new("/proc/pressure").exists());
    }
}
