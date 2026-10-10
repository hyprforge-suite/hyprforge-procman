//! The Recorded tab: the recorder's journal, a day at a time.
//!
//! Three graphs across the day — CPU, memory, I/O pressure — with the
//! strained stretches marked. Pressing on them picks a moment, and the
//! tab shows the processes busiest in that interval and what the stretch
//! around it added up to. Read-only: nothing here acts on a process,
//! because none of them is the process running now.
//!
//! The journal is root's. Opening the tab asks the helper without a
//! prompt (polkit may already allow it); if that is refused, a button
//! asks with one.

use crate::details::duration;
use crate::view::bytes;
use crate::{App, Message};
use hyprforge_procman::graph::DayGraph;
use hyprforge_procman::moment::Row;
use hyprforge_procman::recorded::{self, At, Day, Timeline};
use hyprforge_procman::report::Report;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{canvas, column, container, row, scrollable, Space};
use iced::{Alignment, Element, Length, Task};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// How much of the day around the moment "What happened" adds up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Span {
    TenMinutes,
    #[default]
    Hour,
    Day,
}

impl Span {
    const ALL: [Span; 3] = [Span::TenMinutes, Span::Hour, Span::Day];

    fn ms(self) -> i64 {
        match self {
            Span::TenMinutes => 600_000,
            Span::Hour => 3_600_000,
            Span::Day => i64::MAX / 4,
        }
    }

    fn label(self) -> String {
        match self {
            Span::TenMinutes => "10 minutes",
            Span::Hour => "Hour",
            Span::Day => "Whole day",
        }
        .into()
    }
}

/// A day opened: the journal behind a lock (read on worker threads), and
/// what draws it.
#[derive(Clone)]
pub struct Loaded {
    name: String,
    day: Arc<Mutex<Day>>,
    timeline: Arc<Timeline>,
    /// The newest moment and stretch asked for. A pick made while an older
    /// one is still being read makes the older one skip its work, so a
    /// drag across the day doesn't queue a read for every pixel.
    wanted_at: Arc<AtomicI64>,
    wanted_window: Arc<Mutex<(i64, i64)>>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded").field("name", &self.name).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Ask for the days; `true` lets polkit prompt.
    Open(bool),
    Days(Result<Vec<(String, u64)>, String>),
    Day(String),
    Loaded(Result<Loaded, String>),
    At(i64),
    Moment(i64, Result<Arc<At>, String>),
    Span(Span),
    Summary((i64, i64), Result<Arc<Report>, String>),
    /// A read skipped because a newer pick came first.
    Overtaken,
}

type Summed = Result<Arc<Report>, String>;

#[derive(Default)]
pub struct RecordedTab {
    days: Option<Result<Vec<(String, u64)>, String>>,
    /// Waiting on polkit or the disk.
    busy: bool,
    /// Asked once without a prompt; the button asks with one.
    looked: bool,
    loaded: Option<Result<Loaded, String>>,
    at: Option<i64>,
    span: Span,
    moment: Option<Result<Arc<At>, String>>,
    /// The stretch added up, and what it came to.
    summary: Option<((i64, i64), Summed)>,
}

#[cfg(test)]
impl RecordedTab {
    /// The tab with `path` open and a moment `fraction` of the way across
    /// its day, read here and now rather than on workers — for drawing.
    pub fn read_now(path: &std::path::Path, fraction: f32) -> RecordedTab {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let size = std::fs::metadata(path).unwrap().len();
        let (mut day, timeline) = Day::load(name.clone(), hyprforge_procman::journal::Reader::open(path).unwrap()).unwrap();
        let at = timeline.time_at(fraction);
        let span = Span::default();
        let window = recorded::window(at, span.ms(), timeline.first_ms, timeline.last_ms);
        let moment = day.at(at).map(Arc::new).map_err(|e| e.to_string());
        let summary = day.summary(window.0, window.1, 12).map(Arc::new).map_err(|e| e.to_string());
        let loaded = Loaded { name: name.clone(), day: Arc::new(Mutex::new(day)), timeline: Arc::new(timeline), wanted_at: Arc::default(), wanted_window: Arc::default() };
        RecordedTab { days: Some(Ok(vec![(name, size)])), looked: true, loaded: Some(Ok(loaded)), at: Some(at), span, moment: Some(moment), summary: Some((window, summary)), ..RecordedTab::default() }
    }
}

/// Runs a read of the journal on a worker thread. `None` is a read
/// skipped because a newer one was asked for — or a worker that died,
/// which leaves the last answer showing rather than an error over it.
async fn worker<T: Send + 'static>(f: impl FnOnce() -> Option<T> + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok().flatten()
}

/// Opens a day, and reads its timeline on a worker thread.
async fn load(name: String) -> Result<Loaded, String> {
    let reader = recorded::open(&name, &recorded::directory(), true).await?;
    let n = name.clone();
    let (day, timeline) = tokio::task::spawn_blocking(move || Day::load(n, reader)).await.map_err(|e| format!("the reader stopped: {e}"))?.map_err(|e| e.to_string())?;
    Ok(Loaded { name, day: Arc::new(Mutex::new(day)), timeline: Arc::new(timeline), wanted_at: Arc::default(), wanted_window: Arc::default() })
}

impl App {
    /// The tab was opened: look without prompting, once.
    pub fn recorded_opened(&mut self) -> Task<Message> {
        if self.recorded.looked {
            return Task::none();
        }
        self.recorded.looked = true;
        self.recorded_update(Msg::Open(false))
    }

    pub fn recorded_update(&mut self, msg: Msg) -> Task<Message> {
        let r = &mut self.recorded;
        match msg {
            Msg::Open(interactive) => {
                r.busy = true;
                return Task::perform(async move { recorded::days(&recorded::directory(), interactive).await }, |d| Message::Recorded(Msg::Days(d)));
            }
            Msg::Days(days) => {
                r.busy = false;
                let newest = days.as_ref().ok().and_then(|d| d.first()).map(|(name, _)| name.clone());
                r.days = Some(days);
                if let Some(name) = newest {
                    return self.recorded_update(Msg::Day(name));
                }
            }
            Msg::Day(name) => {
                r.busy = true;
                r.moment = None;
                r.summary = None;
                return Task::perform(load(name), |l| Message::Recorded(Msg::Loaded(l)));
            }
            Msg::Loaded(loaded) => {
                r.busy = false;
                let last = loaded.as_ref().ok().map(|l| l.timeline.last_ms);
                r.loaded = Some(loaded);
                if let Some(last) = last {
                    return self.recorded_update(Msg::At(last));
                }
            }
            Msg::At(at) => {
                r.at = Some(at);
                let Some(Ok(l)) = &r.loaded else { return Task::none() };
                l.wanted_at.store(at, Ordering::SeqCst);
                let (day, wanted) = (l.day.clone(), l.wanted_at.clone());
                let moment = Task::perform(
                    worker(move || {
                        // Overtaken by a newer pick: skip the read.
                        if wanted.load(Ordering::SeqCst) != at {
                            return None;
                        }
                        Some(day.lock().map_err(|_| "the reader stopped".to_string()).and_then(|mut d| d.at(at).map(Arc::new).map_err(|e| e.to_string())))
                    }),
                    move |m| Message::Recorded(m.map_or(Msg::Overtaken, |m| Msg::Moment(at, m))),
                );
                return Task::batch([moment, self.recorded_summary()]);
            }
            Msg::Moment(at, result) => {
                // Only the newest pick's answer.
                if r.at == Some(at) {
                    r.moment = Some(result);
                }
            }
            Msg::Overtaken => {}
            Msg::Span(span) => {
                r.span = span;
                return self.recorded_summary();
            }
            Msg::Summary(window, result) => {
                if self.recorded_window() == Some(window) {
                    self.recorded.summary = Some((window, result));
                }
            }
        }
        Task::none()
    }

    /// The stretch "What happened" covers now.
    fn recorded_window(&self) -> Option<(i64, i64)> {
        let Some(Ok(l)) = &self.recorded.loaded else { return None };
        let at = self.recorded.at?;
        Some(recorded::window(at, self.recorded.span.ms(), l.timeline.first_ms, l.timeline.last_ms))
    }

    /// Adds up the stretch, unless it is the one already added up — a
    /// whole day is read once, not again for every moment picked in it.
    fn recorded_summary(&mut self) -> Task<Message> {
        let Some(window) = self.recorded_window() else { return Task::none() };
        if self.recorded.summary.as_ref().is_some_and(|(w, _)| *w == window) {
            return Task::none();
        }
        let Some(Ok(l)) = &self.recorded.loaded else { return Task::none() };
        if let Ok(mut w) = l.wanted_window.lock() {
            *w = window;
        }
        let (day, wanted) = (l.day.clone(), l.wanted_window.clone());
        Task::perform(
            worker(move || {
                if wanted.lock().map_or(true, |w| *w != window) {
                    return None;
                }
                Some(day.lock().map_err(|_| "the reader stopped".to_string()).and_then(|mut d| d.summary(window.0, window.1, 12).map(Arc::new).map_err(|e| e.to_string())))
            }),
            move |m| Message::Recorded(m.map_or(Msg::Overtaken, |m| Msg::Summary(window, m))),
        )
    }

    pub fn recorded_view(&self, s: FontScale) -> Element<'_, Message> {
        let r = &self.recorded;
        let pad = |e: Element<'static, Message>| -> Element<'_, Message> { container(e).padding(spacing::LG).into() };
        let days = match &r.days {
            None if r.busy => return pad(widgets::hint_text("Looking for the recorded history…", s).into()),
            None => return pad(widgets::hint_text("Looking for the recorded history…", s).into()),
            Some(Err(e)) => {
                // Refused without a prompt is the expected first answer
                // for anyone not already allowed: offer the prompt.
                let mut c = column![widgets::hint_text(format!("{e}."), s)].spacing(spacing::MD);
                if e != recorded::NOT_RECORDING {
                    c = c.push(widgets::primary_button("Open the recorded history").on_press_maybe((!r.busy).then_some(Message::Recorded(Msg::Open(true)))));
                    c = c.push(widgets::meta_text("The recorder's history holds every process on this machine, so reading it asks for an administrator's password.", density::META_TEXT_BASE, s));
                }
                return container(c).padding(spacing::LG).into();
            }
            Some(Ok(d)) if d.is_empty() => return pad(widgets::hint_text("The recorder is on, but hasn't written a day yet.", s).into()),
            Some(Ok(d)) => d,
        };
        let chosen = r.loaded.as_ref().and_then(|l| l.as_ref().ok()).map(|l| l.name.clone());
        let names: Vec<String> = days.iter().take(7).map(|(n, _)| n.clone()).collect();
        let day_choice = widgets::segmented_choice(&names, chosen.as_ref(), |n| day_label(n), |n| Message::Recorded(Msg::Day(n)), s);
        let span_choice = widgets::segmented_choice(&Span::ALL, Some(&r.span), |sp| sp.label(), |sp| Message::Recorded(Msg::Span(sp)), s);
        let bar = row![day_choice, Space::new().width(Length::Fill), span_choice].spacing(spacing::MD).align_y(Alignment::Center);

        let l = match &r.loaded {
            None => return column![container(bar).padding(spacing::LG), pad(widgets::hint_text("Reading the day…", s).into())].into(),
            Some(Err(e)) => return column![container(bar).padding(spacing::LG), pad(widgets::hint_text(format!("Couldn't read that day: {e}."), s).into())].into(),
            Some(Ok(l)) => l,
        };
        let t: &Timeline = &l.timeline;
        let window = self.recorded_window();
        let label_width = s.apply(130.0);
        let at = r.at;
        let graph = |label: &'static str, points: fn(&Timeline) -> &[(i64, f32)]| day_graph(t, label, points(t), at, window, label_width, s);
        let meta = |v: String| widgets::meta_text(v, density::META_TEXT_BASE, s);
        let axis = row![
            Space::new().width(Length::Fixed(label_width + spacing::MD)),
            meta(clock(t.first_ms)),
            Space::new().width(Length::Fill),
            meta(clock((t.first_ms + t.last_ms) / 2)),
            Space::new().width(Length::Fill),
            meta(clock(t.last_ms)),
        ];
        let strained = match t.strained.len() {
            0 => "Memory and I/O pressure stayed under 40 % all day.".to_string(),
            n => format!("Shaded: {n} stretch{} where memory or I/O pressure reached 40 % — something was waiting.", if n == 1 { "" } else { "es" }),
        };
        let graphs = column![graph("CPU busy", |t| &t.cpu), graph("Memory in use", |t| &t.memory), graph("I/O pressure", |t| &t.io), axis, meta(strained), meta("Press or drag on a graph to pick a moment.".into())].spacing(spacing::SM);

        let moment_title = at.map_or_else(|| "The moment".to_string(), |a| format!("At {}", clock(a)));
        let summary_title = window.map_or_else(|| "What happened".to_string(), |(a, b)| format!("What happened, {} – {}", clock(a), clock(b)));
        scrollable(
            column![
                bar,
                widgets::section("The day", s, graphs),
                widgets::section(moment_title, s, self.recorded_moment(s)),
                widgets::section(summary_title, s, self.recorded_happened(s)),
            ]
            .spacing(spacing::LG)
            .padding(spacing::LG),
        )
        .height(Length::Fill)
        .into()
    }

    /// The processes busiest in the interval picked.
    fn recorded_moment(&self, s: FontScale) -> Element<'_, Message> {
        let at = match &self.recorded.moment {
            None => return widgets::hint_text("Reading…", s).into(),
            Some(Err(e)) => return widgets::hint_text(format!("Couldn't read that moment: {e}."), s).into(),
            Some(Ok(at)) => at,
        };
        let m = &at.moment;
        let c = m.system.cpu;
        let busy = (100.0 - c[3] - c[4]).max(0.0) / 100.0;
        let memory = 1.0 - at.available_memory as f32 / at.total_memory.max(1) as f32;
        let mut headline = format!("CPU {} busy · memory {} in use", share(busy), share(memory));
        if let Some(p) = &at.pressure {
            headline.push_str(&format!(" · I/O pressure {:.0} % · memory pressure {:.0} %", p.io_some.avg10, p.memory_some.avg10));
        }
        headline.push_str(&format!(" · {} processes, {} ended", m.system.processes, m.system.exited));
        let mut rows: Vec<&Row> = m.rows.iter().filter(|r| r.cpu_pct > 0.0 || r.read.unwrap_or(0) + r.written.unwrap_or(0) > 0 || r.exit.is_some()).collect();
        rows.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct).then(b.resident_bytes.cmp(&a.resident_bytes)));
        let mut table = column![header(&["Process", "PID", "User", "CPU", "Memory", "Read", "Written", "Network"], s)].spacing(spacing::SM);
        for r in rows.into_iter().take(12) {
            table = table.push(cells(
                row_name(r),
                &[r.id.pid.to_string(), r.user.clone(), format!("{:.1} %", r.cpu_pct), bytes(r.resident_bytes), r.read.map_or_else(|| "—".into(), bytes), r.written.map_or_else(|| "—".into(), bytes), network(r.net_rx, r.net_tx)],
                s,
            ));
        }
        column![widgets::scaled_text(headline, density::ROW_TEXT_BASE, s), table].spacing(spacing::MD).into()
    }

    /// The stretch around the moment, added up.
    fn recorded_happened(&self, s: FontScale) -> Element<'_, Message> {
        let report = match &self.recorded.summary {
            None => return widgets::hint_text("Adding it up…", s).into(),
            Some((_, Err(e))) => return widgets::hint_text(format!("Couldn't add it up: {e}."), s).into(),
            Some((_, Ok(r))) => r,
        };
        if report.intervals == 0 {
            return widgets::hint_text("Nothing was recorded in this stretch.", s).into();
        }
        let cpus = report.cpus.max(1) as f32 * 100.0;
        let seconds: f64 = report.cpu.iter().map(|l| l.seconds).sum::<f64>().max(0.001);
        let mut said = Vec::new();
        if let Some(c) = report.cpu.first() {
            said.push(format!("CPU averaged {} busy", share(1.0 - (c.idle + c.wait) / cpus)));
        }
        if let Some(m) = report.mem.first() {
            said.push(format!("memory got down to {} available", bytes(m.least_available)));
        }
        if let Some(p) = report.psi.first() {
            said.push(format!("I/O pressure peaked at {:.0} %, memory pressure at {:.0} %", p.io.max, p.memory.max));
        }
        if let Some(m) = report.mem.first().filter(|m| m.oom_kills > 0) {
            said.push(format!("{} process{} killed for memory", m.oom_kills, if m.oom_kills == 1 { " was" } else { "es were" }));
        }
        let mut table = column![header(&["Program", "Processes", "CPU time", "Average", "Peak memory", "Read", "Written", "Network"], s)].spacing(spacing::SM);
        // A program that used nothing worth a digit is filler, not news.
        for c in report.top.iter().filter(|c| c.cpu_seconds >= 0.5 || c.read + c.written + c.received.unwrap_or(0) + c.sent.unwrap_or(0) > 0) {
            let average = c.cpu_seconds / (seconds * f64::from(report.cpus.max(1))) * 100.0;
            table = table.push(cells(
                c.name.clone(),
                &[c.processes.to_string(), duration(c.cpu_seconds.round() as u64), format!("{average:.1} %"), bytes(c.peak_resident), bytes(c.read), bytes(c.written), network(c.received, c.sent)],
                s,
            ));
        }
        let sentence = {
            let mut t = said.join("; ");
            if let Some(first) = t.get(..1) {
                t = first.to_uppercase() + &t[1..];
            }
            t + "."
        };
        column![widgets::scaled_text(sentence, density::ROW_TEXT_BASE, s), table].spacing(spacing::MD).into()
    }
}

/// One measure across the day, its value at the moment beside it.
fn day_graph<'a>(t: &'a Timeline, label: &'static str, points: &'a [(i64, f32)], at: Option<i64>, window: Option<(i64, i64)>, label_width: f32, s: FontScale) -> Element<'a, Message> {
    let value = at.and_then(|a| recorded::value_at(points, a)).map_or_else(|| "—".into(), share);
    // The picked moment is a selection, so it takes the accent; the lines
    // are the foreground colour, as every graph in this app is.
    let accent = hyprforge_ui::color::to_iced(theme::active().accent);
    row![
        column![widgets::meta_text(label, density::META_TEXT_BASE, s), widgets::scaled_text(value, density::ROW_TEXT_BASE, s).font(theme::mono_font())]
            .spacing(2.0)
            .width(Length::Fixed(label_width)),
        canvas(DayGraph { timeline: t, points, at, window, line: theme::text(), grid: surface::card_border(), strain: theme::warning(), mark: accent, on_pick: |at| Message::Recorded(Msg::At(at)) })
            .width(Length::Fill)
            .height(Length::Fixed(s.apply(56.0))),
    ]
    .spacing(spacing::MD)
    .align_y(Alignment::Center)
    .into()
}

/// The figure columns' widths, after a name that takes the rest.
const WIDTHS: [f32; 7] = [90.0, 100.0, 90.0, 100.0, 90.0, 90.0, 110.0];

/// In and out together — the column has room for one figure. A dash where
/// nothing was counting, which is not the same as nothing sent.
fn network(rx: Option<u64>, tx: Option<u64>) -> String {
    match (rx, tx) {
        (None, None) => "—".into(),
        (rx, tx) => bytes(rx.unwrap_or(0) + tx.unwrap_or(0)),
    }
}

fn header<'a>(titles: &[&'static str], s: FontScale) -> Element<'a, Message> {
    let mut r = row![widgets::meta_text(titles[0], density::META_TEXT_BASE, s).width(Length::Fill)].spacing(spacing::SM);
    for (t, w) in titles[1..].iter().zip(WIDTHS) {
        r = r.push(container(widgets::meta_text(*t, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End));
    }
    r.into()
}

fn cells<'a>(name: String, figures: &[String], s: FontScale) -> Element<'a, Message> {
    let mut r = row![widgets::scaled_text(name, density::ROW_TEXT_BASE, s).width(Length::Fill)].spacing(spacing::SM).align_y(Alignment::Center);
    for (f, w) in figures.iter().zip(WIDTHS) {
        r = r.push(container(widgets::scaled_text(f.clone(), density::ROW_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End));
    }
    r.into()
}


/// `Today`, `Yesterday`, else `Wed 7`.
fn day_label(name: &str) -> String {
    let Ok(day) = chrono::NaiveDate::parse_from_str(name, "%Y-%m-%d") else { return name.to_owned() };
    let today = chrono::Local::now().date_naive();
    match (today - day).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => day.format("%a %-d").to_string(),
    }
}

fn clock(at_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(at_ms).single().map_or_else(|| "?".into(), |t| t.format("%H:%M:%S").to_string())
}

fn share(v: f32) -> String {
    format!("{:.0} %", v * 100.0)
}

/// A process's row at the moment, with its name in brackets if it ended
/// during the interval — atop's way.
fn row_name(r: &Row) -> String {
    if r.exit.is_some() {
        format!("<{}>", r.name)
    } else {
        r.name.clone()
    }
}
