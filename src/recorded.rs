//! The recorder's journal, in the window: a day drawn as a timeline, any
//! moment on it, and what a stretch of it added up to.
//!
//! Plain data and decisions over it, so all of it is testable without a
//! window. The timeline is `report::build` at one-minute steps and a
//! moment is `moment::between`, so the window, `hyprforge-top -r` and
//! `hyprforge-top report` all read the same figures off the same file.
//! Opening a day the user may not read goes through the helper, which
//! checks polkit first — the one async part, behind `dbus`.

use crate::journal::{self, Reader};
use crate::machine::PressureDetail;
use crate::moment::{self, Moment};
use crate::report::{self, Options, Report};
use crate::users::Users;
use std::io;
use std::path::Path;

/// One minute: the timeline's step. A day is 1440 points, which is more
/// than any window is wide, and the journal's ten seconds would be six
/// times as many for nothing a screen can show.
pub const STEP_MS: i64 = 60_000;

/// A day's journal, open.
pub struct Day {
    pub name: String,
    reader: Reader,
    users: Users,
}

/// What draws the day: each measure as a share from 0 to 1 at the end of
/// each minute, and the stretches where the machine was under strain.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub first_ms: i64,
    pub last_ms: i64,
    /// The share of the CPUs that were busy — not idle, not waiting.
    pub cpu: Vec<(i64, f32)>,
    /// The share of memory in use: total less what is available.
    pub memory: Vec<(i64, f32)>,
    /// I/O pressure: the share of time something waited on storage.
    pub io: Vec<(i64, f32)>,
    /// Where memory or I/O pressure reached [`STRAIN`], merged into
    /// stretches — the marks that say "look here".
    pub strained: Vec<(i64, i64)>,
}

/// The pressure, in percent of the time, that marks a stretch as strained:
/// atop's own red line for PSI.
pub const STRAIN: f32 = 40.0;

/// One moment of the day: the interval ending at a sample.
#[derive(Debug, Clone, Default)]
pub struct At {
    pub moment: Moment,
    pub cpus: u32,
    pub total_memory: u64,
    pub available_memory: u64,
    pub pressure: Option<PressureDetail>,
}

impl Timeline {
    pub fn of(report: &Report, first_ms: i64, last_ms: i64) -> Timeline {
        let cpus = report.cpus.max(1) as f32 * 100.0;
        let cpu = report.cpu.iter().map(|l| (l.at_ms, (1.0 - (l.idle + l.wait) / cpus).clamp(0.0, 1.0))).collect();
        let memory = report.mem.iter().map(|l| (l.at_ms, 1.0 - l.available as f32 / l.total.max(1) as f32)).collect();
        let io = report.psi.iter().map(|l| (l.at_ms, (l.io.mean / 100.0).clamp(0.0, 1.0))).collect();
        let mut strained: Vec<(i64, i64)> = Vec::new();
        let step = (report.step_seconds * 1000.0) as i64;
        for l in report.psi.iter().filter(|l| l.memory.max >= STRAIN || l.io.max >= STRAIN) {
            let from = l.at_ms - step;
            match strained.last_mut() {
                Some(last) if last.1 >= from => last.1 = l.at_ms,
                _ => strained.push((from, l.at_ms)),
            }
        }
        Timeline { first_ms, last_ms, cpu, memory, io, strained }
    }

    /// Where on the day `fraction` of the way across is.
    pub fn time_at(&self, fraction: f32) -> i64 {
        self.first_ms + ((self.last_ms - self.first_ms) as f64 * f64::from(fraction.clamp(0.0, 1.0))) as i64
    }

    /// How far across the day `at_ms` is, from 0 to 1.
    pub fn fraction(&self, at_ms: i64) -> f32 {
        let span = (self.last_ms - self.first_ms).max(1) as f64;
        ((at_ms - self.first_ms) as f64 / span).clamp(0.0, 1.0) as f32
    }
}

/// A measure at `at_ms`: the minute it falls in, or the last there is.
pub fn value_at(points: &[(i64, f32)], at_ms: i64) -> Option<f32> {
    let i = points.partition_point(|&(t, _)| t < at_ms);
    points.get(i).or(points.last()).map(|&(_, v)| v)
}

/// The stretch of `span_ms` around `at_ms`, kept inside the day: centred
/// where it can be, and slid in from an edge where it can't, so ten
/// minutes is always ten minutes.
pub fn window(at_ms: i64, span_ms: i64, first_ms: i64, last_ms: i64) -> (i64, i64) {
    let span = span_ms.min(last_ms - first_ms).max(0);
    let from = (at_ms - span / 2).clamp(first_ms, last_ms - span);
    (from, from + span)
}

impl Day {
    /// Opens a journal and reads its timeline — every frame of the day,
    /// so off the UI thread.
    pub fn load(name: String, mut reader: Reader) -> io::Result<(Day, Timeline)> {
        let (Some(first), Some(last)) = (reader.entries.first(), reader.entries.last()) else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "nothing has been recorded on that day yet"));
        };
        let (first_ms, last_ms) = (first.at_ms, last.at_ms);
        let report = report::build(&mut reader, &Options { from_ms: first_ms, to_ms: last_ms, step_ms: STEP_MS, top: 0 })?;
        let timeline = Timeline::of(&report, first_ms, last_ms);
        Ok((Day { name, reader, users: Users::default() }, timeline))
    }

    /// The interval ending at the first sample at or after `at_ms` — the
    /// sample after it if `at_ms` falls between two.
    pub fn at(&mut self, at_ms: i64) -> io::Result<At> {
        // Never index 0 when there is a sample after it: the first sample
        // has nothing before it in the file, so its interval is since boot.
        let last = self.reader.entries.len() - 1;
        let i = self.reader.entries.partition_point(|e| e.at_ms < at_ms).clamp(last.min(1), last);
        let now = self.reader.frame(i)?;
        let prev = match i.checked_sub(1) {
            Some(p) => self.reader.frame(p)?,
            None => journal::Frame { at_ms: now.snapshot.boot as i64 * 1000, boot_id: now.boot_id.clone(), ..journal::Frame::default() },
        };
        let moment = moment::between(&prev, &now, self.reader.header.ticks_per_second, &mut self.users);
        Ok(At { moment, cpus: self.reader.header.cpus, total_memory: now.snapshot.memory.total, available_memory: now.snapshot.memory.available, pressure: now.machine.pressure_detail })
    }

    /// What `from`..`to` added up to, as one bucket. (A step the length
    /// of the range would not do: buckets fall on multiples of the step,
    /// so an hour from 14:30 would split at 15:00.)
    pub fn summary(&mut self, from_ms: i64, to_ms: i64, top: usize) -> io::Result<Report> {
        report::build(&mut self.reader, &Options { from_ms, to_ms, step_ms: i64::MAX / 4, top })
    }
}

/// The recorder's directory, as its config names it.
pub fn directory() -> std::path::PathBuf {
    crate::recorder::load_config(Path::new(crate::recorder::CONFIG_PATH)).map(|c| c.directory).unwrap_or_else(|_| crate::recorder::Config::default().directory)
}

/// The days recorded, newest first: read directly when this user may,
/// otherwise asked of the helper. Not `interactive`, polkit answers from
/// what it already allows and never puts up a password prompt — for
/// looking without being asked to.
#[cfg(feature = "dbus")]
pub async fn days(directory: &Path, interactive: bool) -> Result<Vec<(String, u64)>, String> {
    let mut days = match journal::days(directory) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => helper(|p| async move { p.history_days(interactive).await }).await?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(NOT_RECORDING.into()),
        Err(e) => return Err(e.to_string()),
    };
    days.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(days)
}

/// Said when there is no journal at all.
pub const NOT_RECORDING: &str = "Nothing has been recorded: the recorder isn't on. Set up's \"Record system history\" switches it on";

/// A day's journal: opened directly when this user may, otherwise handed
/// over by the helper as an open file.
#[cfg(feature = "dbus")]
pub async fn open(day: &str, directory: &Path, interactive: bool) -> Result<Reader, String> {
    if !journal::is_day(day) {
        return Err(format!("{day:?} isn't a day"));
    }
    match Reader::open(&directory.join(format!("{day}.hfr"))) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let day = day.to_owned();
            let fd = helper(|p| async move { p.open_history(&day, interactive).await }).await?;
            Reader::from_file(std::fs::File::from(std::os::fd::OwnedFd::from(fd))).map_err(|e| e.to_string())
        }
        other => other.map_err(|e| e.to_string()),
    }
}

/// One call to the helper on the system bus, its refusal put into words.
#[cfg(feature = "dbus")]
async fn helper<T, F, Fut>(call: F) -> Result<T, String>
where
    F: FnOnce(crate::helper::ProcManProxy<'static>) -> Fut,
    Fut: std::future::Future<Output = zbus::Result<T>>,
{
    let words = |e: zbus::Error| {
        let text = e.to_string();
        if text.contains("ServiceUnknown") || text.contains("was not provided") {
            "the journal is root's and the privileged helper isn't installed".into()
        } else if text.contains("not authorized") || text.contains("AccessDenied") {
            "the journal is root's and administrator access wasn't given".into()
        } else {
            text
        }
    };
    // May prompt for a password, so a person's time — but bounded.
    crate::bound::within(crate::bound::PROMPT, "the privileged helper", async {
        let bus = zbus::Connection::system().await.map_err(words)?;
        let proxy = crate::helper::ProcManProxy::new(&bus).await.map_err(words)?;
        call(proxy).await.map_err(words)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{CpuLine, MemLine, Pressure, PsiLine};

    fn report() -> Report {
        let cpu = |at_ms, idle, wait| CpuLine { at_ms, idle, wait, ..CpuLine::default() };
        let psi = |at_ms, io: f32| PsiLine { at_ms, io: Pressure { mean: io / 2.0, max: io }, ..PsiLine::default() };
        Report {
            cpus: 4,
            step_seconds: 60.0,
            cpu: vec![cpu(60_000, 400.0, 0.0), cpu(120_000, 100.0, 100.0)],
            mem: vec![MemLine { at_ms: 60_000, total: 100, available: 75, ..MemLine::default() }],
            psi: vec![psi(60_000, 10.0), psi(120_000, 50.0), psi(180_000, 60.0), psi(240_000, 0.0), psi(300_000, 45.0)],
            ..Report::default()
        }
    }

    #[test]
    fn busy_is_what_is_neither_idle_nor_waiting() {
        let t = Timeline::of(&report(), 0, 300_000);
        assert_eq!(t.cpu, [(60_000, 0.0), (120_000, 0.5)]);
        assert_eq!(t.memory, [(60_000, 0.25)]);
    }

    #[test]
    fn strained_minutes_merge_into_stretches() {
        let t = Timeline::of(&report(), 0, 300_000);
        assert_eq!(t.strained, [(60_000, 180_000), (240_000, 300_000)], "two minutes over the line, a calm one, then another");
    }

    #[test]
    fn a_measure_is_read_at_the_minute_a_moment_falls_in() {
        let points = [(60, 0.1), (120, 0.2)];
        assert_eq!([value_at(&points, 30), value_at(&points, 61), value_at(&points, 500), value_at(&[], 5)], [Some(0.1), Some(0.2), Some(0.2), None]);
    }

    #[test]
    fn a_window_stays_inside_the_day_and_keeps_its_length() {
        assert_eq!(window(50, 20, 0, 100), (40, 60));
        assert_eq!(window(2, 20, 0, 100), (0, 20), "slid in from the start");
        assert_eq!(window(99, 20, 0, 100), (80, 100), "and from the end");
        assert_eq!(window(50, 500, 0, 100), (0, 100), "a span longer than the day is the day");
    }

    #[test]
    fn a_fraction_of_the_day_and_back() {
        let t = Timeline { first_ms: 1000, last_ms: 2000, ..Timeline::default() };
        assert_eq!((t.time_at(0.25), t.fraction(1250)), (1250, 0.25));
        assert_eq!((t.time_at(7.0), t.fraction(0)), (2000, 0.0), "clamped both ways");
    }

    #[test]
    fn a_day_opens_to_its_timeline_and_any_moment_on_it() {
        use crate::journal::{Frame, Header, Writer};
        use crate::proc::{CpuTimes, Proc, Snapshot};
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("2026-10-10.hfr");
        let header = Header { hostname: "hogwarts".into(), cpus: 4, page_size: 4096, ticks_per_second: 100, interval_seconds: 10, command_lines: true };
        let mut w = Writer::open(&path, &header).unwrap();
        for n in 1..=13u64 {
            let p = Proc { pid: 7, start: 3, name: "blender".into(), ticks: 100 * n, ..Proc::default() };
            let snapshot = Snapshot { procs: vec![p], cpu: CpuTimes { total: 1000 * n, ..CpuTimes::default() }, boot: 1, ..Snapshot::default() };
            w.write(&Frame { at_ms: n as i64 * 10_000, boot_id: "b".into(), snapshot, ..Frame::default() }).unwrap();
        }
        let (mut day, timeline) = Day::load("2026-10-10".into(), Reader::open(&path).unwrap()).unwrap();
        assert_eq!((timeline.first_ms, timeline.last_ms, timeline.cpu.len()), (10_000, 130_000, 3), "minutes ending at 1, 2 and 3");
        // 45 s falls between the samples at 40 and 50: the interval ending at 50.
        let at = day.at(45_000).unwrap();
        assert_eq!((at.moment.at_ms, at.moment.rows[0].cpu_pct), (50_000, 10.0));
        assert_eq!(day.at(0).unwrap().moment.at_ms, 20_000, "before the first interval is the first interval");
        let summary = day.summary(25_000, 85_000, 5).unwrap();
        assert_eq!((summary.intervals, summary.cpu.len(), summary.top[0].name.as_str()), (6, 1, "blender"), "one bucket, though the range crosses a minute");
    }
}
