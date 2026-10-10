//! The recorder's journal: what the machine and every process were doing,
//! every ten seconds, for as many days as it is told to keep.
//!
//! **Files.** One a day, `YYYY-MM-DD.hfr`, mode 0600 and owned by root —
//! it holds every user's command lines, which can hold secrets; atop's
//! logs are world-readable and this deliberately is not. A file starts
//! with [`MAGIC`] and a [`Header`]; records follow.
//!
//! **Records.** `len u32 | crc32 u32 | kind u8 | at_ms i64 | payload`,
//! where the payload is a [`Frame`] in postcard, compressed with zstd:
//!
//! - a **keyframe** stands alone, every [`KEYFRAME_EVERY`] frames (ten
//!   minutes at ten seconds) and at the start of every file;
//! - every other frame is compressed **with the last keyframe as its
//!   dictionary**. Ten seconds apart, two frames are nearly the same
//!   bytes, so zstd writes this one as references into the keyframe —
//!   delta encoding without a hand-written delta: no field-by-field
//!   bookkeeping to get wrong, and any moment is at most two records
//!   away (its keyframe, and itself).
//!
//! **Crash safety.** Each record is written whole and `fdatasync`ed. A
//! record cut short by a crash or power loss fails its length or its
//! CRC, and reading stops there: what came before is intact, and the
//! next write starts a fresh keyframe after it.

use crate::cgroups::Cgroup;
use crate::exits::Exit;
use crate::machine::Machine;
use crate::proc::Snapshot;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The format's name and version. A frame is postcard, which has no field
/// names, so any change to what a frame holds is a new version — 2 added
/// each process's network.
pub const MAGIC: &[u8; 8] = b"HFPROCJ2";
/// Every version's magic starts with this.
const FAMILY: &[u8; 7] = b"HFPROCJ";
/// A keyframe every this many frames.
pub const KEYFRAME_EVERY: u32 = 60;
const LEVEL: i32 = 9;
const KIND_KEY: u8 = 1;
const KIND_DELTA: u8 = 2;
/// len + crc + kind + at.
const RECORD_HEAD: usize = 4 + 4 + 1 + 8;

/// What a file says about the machine it was written on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub hostname: String,
    pub cpus: u32,
    pub page_size: u64,
    pub ticks_per_second: u64,
    pub interval_seconds: u32,
    /// Whether command lines were kept (`record_cmdlines`).
    pub command_lines: bool,
}

/// One moment.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Frame {
    /// Milliseconds since the epoch.
    pub at_ms: i64,
    /// `/proc/sys/kernel/random/boot_id`: a change is a reboot, and the
    /// counters started again.
    pub boot_id: String,
    pub snapshot: Snapshot,
    pub machine: Machine,
    pub cgroups: Vec<Cgroup>,
    /// Processes that ended since the last frame.
    pub exits: Vec<Exit>,
}

fn crc(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// Appends frames to one file.
pub struct Writer {
    file: File,
    keyframe: Option<Vec<u8>>,
    since_key: u32,
}

impl Writer {
    /// Opens `path` to append, writing the header first if it is new.
    /// An existing file is scanned so a torn tail is cut off and the
    /// next frame starts a keyframe after the last whole record.
    pub fn open(path: &Path, header: &Header) -> io::Result<Writer> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let exists = path.exists();
        // Never truncate: an existing day file is appended to, after its
        // last whole record.
        let mut file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path)?;
        set_private(&file)?;
        if !exists || file.metadata()?.len() == 0 {
            let head = postcard::to_allocvec(header).map_err(io::Error::other)?;
            file.write_all(MAGIC)?;
            file.write_all(&(head.len() as u32).to_le_bytes())?;
            file.write_all(&head)?;
            file.sync_data()?;
        } else {
            match Reader::open(path) {
                // Cut a torn tail so new records follow a whole one.
                Ok(r) => file.set_len(r.end)?,
                // A day begun by a recorder of another format: kept, under
                // a name of its own, and the day starts a fresh file — a
                // recorder that refused to write until midnight would
                // lose the day to an upgrade.
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    drop(file);
                    let mut aside = path.as_os_str().to_owned();
                    aside.push(".old");
                    std::fs::rename(path, &aside)?;
                    return Writer::open(path, header);
                }
                Err(e) => return Err(e),
            }
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Writer { file, keyframe: None, since_key: 0 })
    }

    pub fn write(&mut self, frame: &Frame) -> io::Result<()> {
        let raw = postcard::to_allocvec(frame).map_err(io::Error::other)?;
        let (kind, payload) = match (&self.keyframe, self.since_key < KEYFRAME_EVERY) {
            (Some(key), true) => {
                let mut c = zstd::bulk::Compressor::with_dictionary(LEVEL, key)?;
                self.since_key += 1;
                (KIND_DELTA, c.compress(&raw)?)
            }
            _ => {
                let compressed = zstd::bulk::compress(&raw, LEVEL)?;
                self.keyframe = Some(raw);
                self.since_key = 1;
                (KIND_KEY, compressed)
            }
        };
        let mut record = Vec::with_capacity(RECORD_HEAD + payload.len());
        record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        record.extend_from_slice(&[0; 4]);
        record.push(kind);
        record.extend_from_slice(&frame.at_ms.to_le_bytes());
        record.extend_from_slice(&payload);
        let sum = crc(&record[8..]);
        record[4..8].copy_from_slice(&sum.to_le_bytes());
        self.file.write_all(&record)?;
        self.file.sync_data()
    }
}

fn set_private(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// Where one record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub at_ms: i64,
    pub keyframe: bool,
    pub offset: u64,
    pub len: u32,
}

/// One file, indexed.
pub struct Reader {
    file: File,
    pub header: Header,
    pub entries: Vec<Entry>,
    /// Where the last whole record ends.
    end: u64,
    /// The decompressor primed with the last keyframe used, by index:
    /// reading a day in order decodes each keyframe once rather than once
    /// for every frame after it.
    keyed: Option<(usize, zstd::bulk::Decompressor<'static>)>,
}

impl Reader {
    pub fn open(path: &Path) -> io::Result<Reader> {
        Reader::from_file(File::open(path)?).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    /// A journal already open — one the helper handed over as a file
    /// descriptor, for a reader who may not open the path itself.
    pub fn from_file(mut file: File) -> io::Result<Reader> {
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            let why = if magic.starts_with(FAMILY) { format!("a journal of format {}, and this reads {}", magic[7] as char, MAGIC[7] as char) } else { "not a procman journal".into() };
            return Err(io::Error::new(io::ErrorKind::InvalidData, why));
        }
        let mut len = [0u8; 4];
        file.read_exact(&mut len)?;
        let mut head = vec![0u8; u32::from_le_bytes(len) as usize];
        file.read_exact(&mut head)?;
        let header: Header = postcard::from_bytes(&head).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let size = file.metadata()?.len();
        let mut entries = Vec::new();
        let mut offset = 8 + 4 + head.len() as u64;
        let mut end = offset;
        // Read every record whole and check it: a torn or corrupted tail
        // ends the index there.
        loop {
            if offset + RECORD_HEAD as u64 > size {
                break;
            }
            let mut rh = [0u8; RECORD_HEAD];
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut rh)?;
            let plen = u32::from_le_bytes(rh[0..4].try_into().unwrap());
            let sum = u32::from_le_bytes(rh[4..8].try_into().unwrap());
            let kind = rh[8];
            let at_ms = i64::from_le_bytes(rh[9..17].try_into().unwrap());
            if offset + RECORD_HEAD as u64 + u64::from(plen) > size || !(kind == KIND_KEY || kind == KIND_DELTA) {
                break;
            }
            let mut body = vec![0u8; plen as usize];
            file.read_exact(&mut body)?;
            let mut checked = rh[8..].to_vec();
            checked.extend_from_slice(&body);
            if crc(&checked) != sum {
                break;
            }
            entries.push(Entry { at_ms, keyframe: kind == KIND_KEY, offset, len: plen });
            offset += RECORD_HEAD as u64 + u64::from(plen);
            end = offset;
        }
        Ok(Reader { file, header, entries, end, keyed: None })
    }

    fn payload(&mut self, e: Entry) -> io::Result<Vec<u8>> {
        let mut body = vec![0u8; e.len as usize];
        self.file.seek(SeekFrom::Start(e.offset + RECORD_HEAD as u64))?;
        self.file.read_exact(&mut body)?;
        Ok(body)
    }

    /// The frame at index `i`.
    pub fn frame(&mut self, i: usize) -> io::Result<Frame> {
        let e = *self.entries.get(i).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such record"))?;
        // Generous: a frame of thousands of processes is a few MB raw.
        const MAX: usize = 256 << 20;
        let raw = if e.keyframe {
            zstd::bulk::decompress(&self.payload(e)?, MAX)?
        } else {
            let key = (0..i).rev().find(|&k| self.entries[k].keyframe).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a frame with no keyframe before it"))?;
            if self.keyed.as_ref().is_none_or(|(k, _)| *k != key) {
                let key_raw = zstd::bulk::decompress(&self.payload(self.entries[key])?, MAX)?;
                self.keyed = Some((key, zstd::bulk::Decompressor::with_dictionary(&key_raw)?));
            }
            let body = self.payload(e)?;
            let (_, d) = self.keyed.as_mut().expect("primed above");
            d.decompress(&body, MAX)?
        };
        postcard::from_bytes(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// The index of the last frame at or before `at_ms`.
    pub fn at(&self, at_ms: i64) -> Option<usize> {
        let i = self.entries.partition_point(|e| e.at_ms <= at_ms);
        i.checked_sub(1)
    }
}

/// Whether `day` is a day file's name, `YYYY-MM-DD` and nothing else — the
/// whole of what the helper will open, so no path can be smuggled in.
pub fn is_day(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The days a journal directory holds, oldest first, with their sizes.
pub fn days(dir: &Path) -> io::Result<Vec<(String, u64)>> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let day = name.strip_suffix(".hfr")?.to_owned();
            is_day(&day).then(|| (day, e.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// The day file for a moment, in the machine's local time.
pub fn day_file(dir: &Path, at: chrono::DateTime<chrono::Local>) -> PathBuf {
    dir.join(format!("{}.hfr", at.format("%Y-%m-%d")))
}

/// Deletes the oldest day files until at most `keep_days` remain and
/// together they take at most `max_bytes`. Today's file is never
/// deleted. Returns what was removed.
pub fn prune(dir: &Path, keep_days: usize, max_bytes: u64, today: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "hfr"))
        .map(|p| {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            (p, size)
        })
        .collect();
    // YYYY-MM-DD sorts by date.
    files.sort();
    let mut total: u64 = files.iter().map(|(_, s)| s).sum();
    let mut removed = Vec::new();
    while let Some((oldest, size)) = files.first().cloned() {
        if oldest == today || (files.len() <= keep_days && total <= max_bytes) {
            break;
        }
        std::fs::remove_file(&oldest)?;
        total -= size;
        removed.push(oldest);
        files.remove(0);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Proc;

    fn header() -> Header {
        Header { hostname: "mcc".into(), cpus: 16, page_size: 4096, ticks_per_second: 100, interval_seconds: 10, command_lines: true }
    }

    fn frame(at_ms: i64, ticks: u64) -> Frame {
        let procs = (1..300)
            .map(|pid| Proc { pid, start: pid as u64 * 7, name: format!("proc-{}", pid % 40), command: format!("/usr/bin/proc-{} --flag", pid % 40), state: 'S', ppid: 1, ticks: ticks + pid as u64, threads: 3, private_bytes: 1 << 20, uid: 1000, ..Proc::default() })
            .collect();
        Frame { at_ms, boot_id: "b1".into(), snapshot: Snapshot { procs, ..Snapshot::default() }, ..Frame::default() }
    }

    #[test]
    fn frames_come_back_exactly_as_written_keyframe_or_not() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("2026-10-10.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        let frames: Vec<Frame> = (0..130).map(|i| frame(i * 10_000, i as u64 * 3)).collect();
        for f in &frames {
            w.write(f).unwrap();
        }
        let mut r = Reader::open(&path).unwrap();
        assert_eq!(r.header, header());
        assert_eq!(r.entries.len(), 130);
        assert_eq!(r.entries.iter().filter(|e| e.keyframe).count(), 3, "at 0, 60 and 120");
        for i in [0usize, 1, 59, 60, 61, 129] {
            assert_eq!(r.frame(i).unwrap(), frames[i], "frame {i}");
        }
    }

    #[test]
    fn a_frame_between_keyframes_is_a_fraction_of_one() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        for i in 0..10 {
            w.write(&frame(i * 10_000, i as u64)).unwrap();
        }
        let r = Reader::open(&path).unwrap();
        let (key, delta) = (r.entries[0].len, r.entries[5].len);
        assert!(delta * 3 < key, "keyframe {key} bytes, a delta {delta}");
    }

    #[test]
    fn seeking_finds_the_last_frame_at_or_before_a_moment() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        for i in 0..5 {
            w.write(&frame(1000 + i * 10_000, 0)).unwrap();
        }
        let r = Reader::open(&path).unwrap();
        assert_eq!(r.at(999), None);
        assert_eq!(r.at(1000), Some(0));
        assert_eq!(r.at(25_000), Some(2));
        assert_eq!(r.at(i64::MAX), Some(4));
    }

    /// Power lost mid-write: the half record is ignored, and the next
    /// write continues after the last whole one, with a keyframe.
    #[test]
    fn a_torn_tail_is_ignored_and_writing_resumes_after_it() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        for i in 0..3 {
            w.write(&frame(i * 10_000, 0)).unwrap();
        }
        drop(w);
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[200, 0, 0, 0, 1, 2, 3]).unwrap();
        drop(f);
        assert_eq!(Reader::open(&path).unwrap().entries.len(), 3);
        let mut w = Writer::open(&path, &header()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), whole, "the tear was cut off");
        w.write(&frame(40_000, 0)).unwrap();
        let mut r = Reader::open(&path).unwrap();
        assert_eq!(r.entries.len(), 4);
        assert!(r.entries[3].keyframe, "a new writer starts with a keyframe");
        assert_eq!(r.frame(3).unwrap().at_ms, 40_000);
    }

    #[test]
    fn a_day_begun_in_an_older_format_is_kept_aside_and_the_day_goes_on() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("2026-10-10.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        w.write(&frame(0, 0)).unwrap();
        drop(w);
        // The same file as a version-1 recorder would have left it.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[7] = b'1';
        std::fs::write(&path, &bytes).unwrap();
        let refused = Reader::open(&path).err().expect("a reader of format 2 cannot read format 1");
        assert!(refused.to_string().contains("format 1"), "{refused}");

        let mut w = Writer::open(&path, &header()).unwrap();
        w.write(&frame(10_000, 0)).unwrap();
        assert_eq!(Reader::open(&path).unwrap().entries.len(), 1, "a fresh file");
        assert_eq!(std::fs::read(t.path().join("2026-10-10.hfr.old")).unwrap(), bytes, "the old one untouched");
        assert_eq!(days(t.path()).unwrap().len(), 1, "and not listed as a day");
    }

    #[test]
    fn a_corrupted_record_ends_the_readable_part() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        for i in 0..3 {
            w.write(&frame(i * 10_000, 0)).unwrap();
        }
        drop(w);
        let r = Reader::open(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = (r.entries[1].offset + RECORD_HEAD as u64 + 5) as usize;
        bytes[mid] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(Reader::open(&path).unwrap().entries.len(), 1, "the CRC caught it");
    }

    /// The real machine: a minute of real frames at ten seconds, and what
    /// a day of them would take. Prints its figures; asserts the plan's
    /// ceiling of 150 MB a day.
    #[test]
    #[ignore = "reads this machine for a minute"]
    fn a_day_of_this_machine_fits_the_budget() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        let mut w = Writer::open(&path, &header()).unwrap();
        let mut fs = crate::proc::ProcFs::default();
        fs.verbose = true;
        fs.threads = true;
        let sys = crate::machine::SysFs::default();
        let frames = 7;
        for i in 0..frames {
            let frame = Frame {
                at_ms: i * 10_000,
                boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default().trim().into(),
                snapshot: fs.snapshot().unwrap(),
                machine: sys.read(),
                cgroups: crate::cgroups::all(Path::new("/sys/fs/cgroup"), 6),
                exits: Vec::new(),
            };
            w.write(&frame).unwrap();
            if i + 1 < frames {
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
        let r = Reader::open(&path).unwrap();
        let key = r.entries[0].len as f64;
        let delta = r.entries[1..].iter().map(|e| e.len as f64).sum::<f64>() / (r.entries.len() - 1) as f64;
        // A day at ten seconds: 144 keyframes and 8496 frames between.
        let per_day = 144.0 * key + 8496.0 * delta;
        eprintln!("keyframe {key:.0} B, between {delta:.0} B, a day ≈ {:.1} MB", per_day / 1e6);
        assert!(per_day < 150e6, "{:.1} MB a day", per_day / 1e6);
    }

    #[test]
    fn only_a_plain_date_names_a_day() {
        assert!(is_day("2026-10-10"));
        for bad in ["../etc/shadow", "2026-10-1", "2026/10/10", "2026-10-10.hfr", "2026-1a-10", ""] {
            assert!(!is_day(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("d.hfr");
        Writer::open(&path, &header()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn pruning_keeps_today_and_drops_the_oldest_first() {
        let t = tempfile::tempdir().unwrap();
        for d in ["2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04"] {
            std::fs::write(t.path().join(format!("{d}.hfr")), vec![0u8; 100]).unwrap();
        }
        let today = t.path().join("2026-10-04.hfr");
        let removed = prune(t.path(), 2, u64::MAX, &today).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!t.path().join("2026-10-01.hfr").exists() && t.path().join("2026-10-03.hfr").exists());
        let removed = prune(t.path(), 30, 150, &today).unwrap();
        assert_eq!(removed, [t.path().join("2026-10-03.hfr")], "over the byte budget");
        assert!(prune(t.path(), 0, 0, &today).unwrap().is_empty(), "today is never removed");
    }
}
