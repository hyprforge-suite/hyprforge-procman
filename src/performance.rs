//! The Performance page: every resource down the left — or across the
//! top, narrow — and the chosen one in full: its name, a minute of it,
//! its facts as tiles, and what is under it (each thread, swap, what is
//! using the battery). Drawn to the 2026-10-18 design.
//!
//! Resources are named by what they are ("WD_BLACK SN770", "GeForce RTX
//! 5070"), not by their index ("Disk 0", "GPU 0 · nvidia"), and grouped,
//! so three drives read as storage rather than three strangers.

use crate::view::{bytes, percent, rate};
use crate::{App, Message, Resource};
use hyprforge_procman::fit::Fit;
use hyprforge_procman::graph::Graph;
use hyprforge_procman::history::Series;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{button, canvas, column, container, row, scrollable, stack, Space};
use iced::{Alignment, Background, Border, Color, Element, Length};

/// One entry in the resource list.
pub struct Item<'a> {
    pub resource: Resource,
    /// The heading it sits under, for the first of its kind.
    pub group: Option<&'static str>,
    pub title: String,
    pub figure: String,
    pub series: &'a Series,
    pub ceiling: f32,
}

/// One fact on a page, drawn as a [`widgets::stat_tile`].
pub struct Fact {
    label: &'static str,
    value: String,
    note: Option<String>,
    warn: bool,
}

fn fact(label: &'static str, value: impl Into<String>) -> Fact {
    Fact { label, value: value.into(), note: None, warn: false }
}

impl Fact {
    fn note(mut self, note: impl Into<String>) -> Fact {
        self.note = Some(note.into());
        self
    }

    fn warn(mut self, warn: bool) -> Fact {
        self.warn = warn;
        self
    }
}

/// `2.9 GHz`, or the MHz figure under a gigahertz.
fn clock(mhz: u32) -> String {
    if mhz >= 1000 { format!("{:.1} GHz", f64::from(mhz) / 1000.0) } else { format!("{mhz} MHz") }
}

/// `1 d 17 h`, `5 h 12 m`, `14 m`: how long the machine has been up,
/// in the two units that matter at that length.
fn uptime(seconds: u64) -> String {
    let (d, h, m) = (seconds / 86_400, seconds % 86_400 / 3600, seconds % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} m"),
        (0, _) => format!("{h} h {m} m"),
        _ => format!("{d} d {h} h"),
    }
}

/// A GPU's name: the driver's own, without the maker it repeats, or the
/// maker's when the driver gives none.
fn gpu_name(maker: &str, reading: Option<&str>) -> String {
    match reading {
        Some(name) => name.trim_start_matches("NVIDIA ").trim_end_matches(" Laptop GPU").to_string(),
        None => format!("{maker} graphics"),
    }
}

impl App {
    pub fn performance(&self, s: FontScale) -> Element<'_, Message> {
        let wide = self.fit() == Fit::Full;
        let items = self.resources();
        let detail = scrollable(container(self.detail(wide, s)).padding(if wide { spacing::LG } else { spacing::MD })).height(Length::Fill);
        if !wide {
            // Across the top, scrolling sideways: each card keeps its
            // figure and its line; only the column it cost is gone.
            let strip = row(items.into_iter().map(|i| self.resource_card(i, Some(150.0), s))).spacing(spacing::SM).padding([spacing::SM, spacing::MD]);
            let strip = scrollable(strip).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(4.0).scroller_width(4.0)));
            return column![strip, widgets::divider(), detail].height(Length::Fill).into();
        }
        let mut rail = column![].spacing(2.0).padding(spacing::SM);
        for item in items {
            if let Some(group) = item.group {
                rail = rail.push(container(widgets::meta_text(group, density::META_TEXT_BASE, s)).padding(iced::Padding { top: spacing::MD, bottom: 0.0, left: spacing::SM, right: 0.0 }));
            }
            rail = rail.push(self.resource_card(item, None, s));
        }
        let rail = container(scrollable(rail)).width(Length::Fixed(s.apply(280.0))).height(Length::Fill);
        row![rail, widgets::vertical_divider(), detail].height(Length::Fill).into()
    }

    /// A resource as a row: its name and figure, and its minute as a
    /// line, chosen the way every list here is.
    fn resource_card<'a>(&'a self, item: Item<'a>, width: Option<f32>, s: FontScale) -> Element<'a, Message> {
        let selected = self.resource == item.resource;
        let ink = if selected { theme::text() } else { theme::text_dim() };
        let name = widgets::clamped_text(item.title, 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill);
        // In the strip a card is narrow, and a figure like "9.2 GB of
        // 60.6 GB" beside the name squeezed the name to nothing: there
        // the figure goes under it.
        let label: Element<'a, Message> = match width {
            Some(_) => column![name, widgets::clamped_text(item.figure, 1).size(s.apply(density::META_TEXT_BASE)).font(theme::mono_font()).color(ink)].spacing(2.0).into(),
            None => row![name, widgets::scaled_text(item.figure, density::META_TEXT_BASE, s).font(theme::mono_font()).color(ink)].spacing(spacing::SM).align_y(Alignment::Center).into(),
        };
        let body = column![
            label,
            canvas(Graph { series: item.series, ceiling: item.ceiling, line: theme::text(), grid: Color::TRANSPARENT, motion: self.motion() })
                .width(Length::Fill)
                .height(Length::Fixed(s.apply(26.0))),
        ]
        .spacing(spacing::XS);
        button(body)
            .padding([spacing::SM, spacing::SM])
            .width(width.map_or(Length::Fill, |w| Length::Fixed(s.apply(w))))
            .on_press(Message::Resource(item.resource))
            .style(move |t, status| widgets::selectable_row_style(t, status, selected))
            .into()
    }

    /// The list's entries, in its order: processor and memory, then each
    /// kind under its heading.
    fn resources(&self) -> Vec<Item<'_>> {
        let t = self.totals;
        let h = &self.history;
        let item = |resource, group, title: String, figure: String, series, ceiling| Item { resource, group, title, figure, series, ceiling };
        let mut out = vec![
            item(Resource::Cpu, None, "Processor".into(), percent(t.cpu.map(|c| c * 100.0)), &h.cpu, 1.0),
            item(Resource::Memory, None, "Memory".into(), format!("{} of {}", bytes(t.memory_used), bytes(t.memory_total)), &h.memory, 1.0),
        ];
        let mut first = Some("Storage");
        for d in &self.machine.disks {
            if let Some(series) = h.disks.get(&d.name) {
                let r = self.disk_rates.iter().find(|(n, _)| *n == d.name).map(|(_, r)| *r).unwrap_or_default();
                out.push(item(Resource::Disk(d.name.clone()), first.take(), d.model.clone().unwrap_or_else(|| d.name.clone()), rate(Some(r.read + r.write)), series, 1.0));
            }
        }
        let mut first = Some("Network");
        for n in &self.machine.nets {
            if let Some(series) = h.nets.get(&n.name) {
                let figure = self.net_rates.iter().find(|(name, _)| *name == n.name).map_or("—".to_string(), |(_, r)| rate(Some(r.rx + r.tx)));
                let title = if n.wireless { format!("Wi-Fi · {}", n.name) } else { format!("Ethernet · {}", n.name) };
                out.push(item(Resource::Net(n.name.clone()), first.take(), title, figure, series, series.ceiling(64.0 * 1024.0)));
            }
        }
        let mut first = Some("Graphics");
        for g in &self.machine.gpus {
            if let Some(series) = h.gpus.get(&g.card) {
                let figure = if g.power.as_deref() == Some("suspended") { "asleep".to_string() } else { percent(self.gpu_busy(g)) };
                out.push(item(Resource::Gpu(g.card.clone()), first.take(), self.gpu_title(g), figure, series, 1.0));
            }
        }
        if let Some(b) = &self.machine.battery {
            let figure = match b.watts.filter(|w| *w > 0.05) {
                Some(w) => format!("{}% · {w:.1} W", b.percent),
                None => format!("{}%", b.percent),
            };
            out.push(item(Resource::Battery, Some("Power"), "Battery".into(), figure, &h.battery, 1.0));
        }
        out
    }

    fn gpu_title(&self, g: &hyprforge_procman::machine::Gpu) -> String {
        let maker = hyprforge_procman::gpu::maker(&g.pci, &self.machine.gpus);
        let reading = (g.driver == "nvidia").then_some(self.nvidia.as_ref()).flatten().and_then(|n| n.as_ref().ok()).map(|r| r.name.as_str());
        gpu_name(maker, reading)
    }

    fn detail(&self, wide: bool, s: FontScale) -> Element<'_, Message> {
        let t = self.totals;
        let m = &self.machine;
        let waited = |share: Option<f32>| share.map_or("—".to_string(), |p| format!("{p:.0}%"));
        let celsius = |c: Option<f32>| c.map_or("—".to_string(), |c| format!("{c:.0} °C"));
        let columns = if wide { 4 } else { 3 };
        match &self.resource {
            Resource::Cpu => {
                let p = &self.processor;
                let threads = self.history.cores.len();
                let mut about: Vec<String> = Vec::new();
                about.extend(p.maker.clone());
                about.push(match p.cores {
                    Some(c) => format!("{threads} threads on {c} cores"),
                    None => format!("{threads} threads"),
                });
                if let (Some(lo), Some(hi)) = (p.min_mhz, p.max_mhz) {
                    about.push(format!("{} – {}", clock(lo), clock(hi)));
                }
                let mut facts = vec![fact("Processes", t.processes.to_string()).note(format!("{} threads", t.threads))];
                if let Some(l) = m.load {
                    facts.push(fact("Load", format!("{:.2}", l[0])).note(format!("{:.2} · {:.2} over 5 · 15 min", l[1], l[2])));
                }
                facts.push(fact("Waiting for CPU", waited(m.pressure.map(|p| p.cpu_some))).note("of the last 10 s"));
                if let Some(up) = m.uptime_seconds {
                    facts.push(fact("Up", uptime(up)).note("since boot"));
                }
                facts.push(fact("Temperature", celsius(m.cpu_celsius)));
                if let Some((first, rest)) = m.fans.split_first() {
                    let others: Vec<String> = rest.iter().map(|f| format!("{} rpm", f.rpm)).collect();
                    let f = fact("Fans", format!("{} rpm", first.rpm));
                    facts.push(if others.is_empty() { f } else { f.note(format!("and {}", others.join(", "))) });
                }
                if let Some(w) = m.gpus.iter().find(|g| g.driver == "amdgpu").and_then(|g| g.watts) {
                    facts.push(fact("Power", format!("{w:.1} W")).note("CPU and GPU together"));
                }
                let iowait = self.overview.split[4];
                facts.push(fact("I/O wait", percent(Some(iowait * 100.0))).note(format!("{} waiting on disk", self.counters.blocked)).warn(iowait > 0.1));

                let o = &self.overview.split;
                let legend = row![
                    key(theme::text(), "all", percent(t.cpu.map(|c| c * 100.0)), s),
                    key(theme::warning(), "kernel", percent(Some((o[2] + o[5] + o[6]) * 100.0)), s),
                ]
                .spacing(spacing::MD);
                let graph = stack![
                    canvas(Graph { series: &self.history.cpu, ceiling: 1.0, line: theme::text(), grid: surface::card_border(), motion: self.motion() }).width(Length::Fill).height(Length::Fixed(s.apply(if wide { 220.0 } else { 170.0 }))),
                    canvas(Graph { series: &self.history.cpu_system, ceiling: 1.0, line: theme::warning(), grid: Color::TRANSPARENT, motion: self.motion() }).width(Length::Fill).height(Length::Fixed(s.apply(if wide { 220.0 } else { 170.0 }))),
                ];
                let title = p.model.clone().unwrap_or_else(|| "Processor".to_string());
                self.page(title, about.join(" · "), percent(t.cpu.map(|c| c * 100.0)), boxed(column![legend, graph].spacing(spacing::SM).into()), facts, columns, Some(self.threads(wide, s)), s)
            }
            Resource::Memory => {
                let mut facts = vec![
                    fact("In use", bytes(t.memory_used)).note(format!("of {}", bytes(t.memory_total))),
                    fact("Available", bytes(t.memory_total.saturating_sub(t.memory_used))),
                    fact("File cache", bytes(m.memory.cached.saturating_sub(m.memory.shmem))).note("given back when needed"),
                    fact("Waiting for memory", waited(m.pressure.map(|p| p.memory_some))).note("of the last 10 s"),
                ];
                if t.swap_total > 0 {
                    facts.push(fact("Swap", bytes(t.swap_used)).note(format!("of {}", bytes(t.swap_total))));
                }
                let swap = (t.swap_total > 0).then(|| column![widgets::section_label("Swap", s), self.graph(&self.history.swap, 1.0, s.apply(80.0))].spacing(spacing::SM).into());
                self.page("Memory".into(), String::new(), bytes(t.memory_used), self.graph(&self.history.memory, 1.0, s.apply(220.0)), facts, columns, swap, s)
            }
            Resource::Disk(name) => {
                let Some(d) = m.disks.iter().find(|d| &d.name == name) else { return self.gone(s) };
                let r = self.disk_rates.iter().find(|(n, _)| n == name).map(|(_, r)| *r).unwrap_or_default();
                let facts = vec![
                    fact("Read", rate(Some(r.read))),
                    fact("Write", rate(Some(r.write))),
                    fact("Active", percent(Some(r.active * 100.0))).note("of the last reading"),
                    fact("Temperature", celsius(d.celsius)),
                    // PSI has no per-device figure: this is every drive's.
                    fact("Waiting for storage", waited(m.pressure.map(|p| p.io_some))).note("every drive, last 10 s"),
                ];
                match self.history.disks.get(name) {
                    Some(series) => self.page(d.model.clone().unwrap_or_else(|| d.name.clone()), format!("{} · active time", d.name), percent(Some(r.active * 100.0)), self.graph(series, 1.0, s.apply(220.0)), facts, columns, None, s),
                    None => self.gone(s),
                }
            }
            Resource::Net(name) => {
                let Some(n) = m.nets.iter().find(|n| &n.name == name) else { return self.gone(s) };
                let r = self.net_rates.iter().find(|(x, _)| x == name).map(|(_, r)| *r).unwrap_or_default();
                let facts = vec![fact("Receiving", rate(Some(r.rx))), fact("Sending", rate(Some(r.tx))), fact("Packets", format!("{:.0}/s in", r.rx_packets)).note(format!("{:.0}/s out", r.tx_packets))];
                match self.history.nets.get(name) {
                    Some(series) => {
                        let ceiling = series.ceiling(64.0 * 1024.0);
                        let title = if n.wireless { "Wi-Fi" } else { "Ethernet" };
                        self.page(title.into(), format!("{} · scale {}", n.name, rate(Some(f64::from(ceiling)))), rate(Some(r.rx + r.tx)), self.graph(series, ceiling, s.apply(220.0)), facts, columns, None, s)
                    }
                    None => self.gone(s),
                }
            }
            Resource::Gpu(card) => {
                let Some(g) = m.gpus.iter().find(|g| &g.card == card) else { return self.gone(s) };
                let asleep = g.power.as_deref() == Some("suspended");
                let nvidia = (g.driver == "nvidia").then_some(self.nvidia.as_ref()).flatten();
                let reading = nvidia.and_then(|n| n.as_ref().ok());
                let mut facts = vec![fact(
                    "Power state",
                    match g.power.as_deref() {
                        Some("suspended") => "asleep",
                        Some("active") => "awake",
                        _ => "—",
                    },
                )
                .note(if asleep { "not woken to be read" } else { "" })];
                let (vram, celsius_now, watts) = match reading {
                    Some(r) => (r.vram_used.zip(r.vram_total), r.celsius, r.watts),
                    None => (g.vram_used.zip(g.vram_total), g.celsius, g.watts),
                };
                if let Some((used, total)) = vram {
                    facts.push(fact("Video memory", bytes(used)).note(format!("of {}", bytes(total))));
                }
                if !asleep {
                    facts.push(fact("Temperature", celsius(celsius_now)));
                }
                if let Some(w) = watts {
                    facts.push(fact("Power", format!("{w:.1} W")));
                }
                if let Some(r) = reading {
                    if let Some(c) = r.clock_mhz {
                        facts.push(fact("Clock", clock(c)).note(format!("of {} · {}", r.max_clock_mhz.map_or("—".to_string(), clock), r.pstate)));
                    }
                } else if let Some(Err(why)) = nvidia {
                    facts.push(fact("Can't read it", "—").note(why.clone()));
                }
                let figure = if asleep { "asleep".to_string() } else { percent(self.gpu_busy(g)) };
                let maker = hyprforge_procman::gpu::maker(&g.pci, &m.gpus);
                match self.history.gpus.get(card) {
                    Some(series) => self.page(self.gpu_title(g), format!("{maker} · {} · {}", g.driver, g.card), figure, self.graph(series, 1.0, s.apply(220.0)), facts, columns, None, s),
                    None => self.gone(s),
                }
            }
            Resource::Battery => {
                let Some(b) = &m.battery else { return self.gone(s) };
                let mut facts = vec![fact("Charge", format!("{}%", b.percent)).note(b.status.clone())];
                if let Some(w) = b.watts.filter(|w| *w > 0.05) {
                    facts.push(fact("Power", format!("{w:.1} W")));
                }
                self.page("Battery".into(), String::new(), format!("{}%", b.percent), self.graph(&self.history.battery, 1.0, s.apply(220.0)), facts, columns, Some(self.power_users(s)), s)
            }
        }
    }

    /// Every thread as a cell: its share in the info colour, deeper the
    /// busier, its own minute as a line, and its clock. Eight to a row,
    /// where sixteen separate graphs took a screen.
    fn threads(&self, wide: bool, s: FontScale) -> Element<'_, Message> {
        let mut grid = column![widgets::meta_text("Each thread", density::META_TEXT_BASE, s)].spacing(spacing::SM);
        for (n, chunk) in self.history.cores.chunks(8).enumerate() {
            let mut r = row![].spacing(spacing::XS);
            for (i, series) in chunk.iter().enumerate() {
                let index = n * 8 + i;
                let used = series.last().unwrap_or(0.0).clamp(0.0, 1.0);
                let hot = used > 0.5;
                let base = theme::info();
                let fill = if hot { base } else { Color { a: 0.06 + used * 0.9, ..base } };
                let ink = if hot { surface::root() } else { theme::text() };
                let sub = if hot { surface::root() } else { theme::text_dim() };
                let mhz = self.machine.clocks.get(index).and_then(|c| c.0).map_or("—".to_string(), clock);
                let mono = |v: String, color: Color| widgets::scaled_text(v, density::META_TEXT_BASE, s).font(theme::mono_font()).color(color);
                let top: Element<'_, Message> = if wide {
                    row![mono(format!("#{index}"), sub), Space::new().width(Length::Fill), mono(format!("{:.0}%", used * 100.0), ink)].into()
                } else {
                    container(mono(format!("{:.0}%", used * 100.0), ink)).width(Length::Fill).align_x(Alignment::Center).into()
                };
                let cell = column![
                    top,
                    canvas(Graph { series, ceiling: 1.0, line: ink, grid: Color::TRANSPARENT, motion: self.motion() }).width(Length::Fill).height(Length::Fixed(s.apply(if wide { 22.0 } else { 14.0 }))),
                    widgets::scaled_text(mhz, density::META_TEXT_BASE * 0.8, s).font(theme::mono_font()).color(sub),
                ]
                .spacing(2.0);
                r = r.push(container(cell).width(Length::FillPortion(1)).padding([spacing::XS, spacing::SM]).style(move |_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(fill)),
                    border: Border { radius: density::inner_radius().into(), ..Border::default() },
                    ..container::Style::default()
                }));
            }
            for _ in chunk.len()..8 {
                r = r.push(Space::new().width(Length::FillPortion(1)));
            }
            grid = grid.push(r);
        }
        grid.into()
    }

    #[allow(clippy::too_many_arguments)]
    fn page<'a>(&'a self, title: String, about: String, figure: String, graph: Element<'a, Message>, facts: Vec<Fact>, columns: usize, below: Option<Element<'a, Message>>, s: FontScale) -> Element<'a, Message> {
        let mut head = row![widgets::clamped_text(title, 1).size(s.apply(22.0)).width(Length::Shrink)].spacing(spacing::MD).align_y(Alignment::Center);
        if !about.is_empty() {
            head = head.push(widgets::clamped_text(about, 1).size(s.apply(density::META_TEXT_BASE)).color(theme::text_dim()).width(Length::Fill));
        } else {
            head = head.push(Space::new().width(Length::Fill));
        }
        head = head.push(widgets::scaled_text(figure, 26.0, s).font(theme::mono_font()));
        let mut tiles = column![].spacing(spacing::MD);
        let mut facts = facts.into_iter().peekable();
        while facts.peek().is_some() {
            let mut r = row![].spacing(spacing::LG);
            let mut n = 0;
            for f in facts.by_ref().take(columns) {
                r = r.push(container(widgets::stat_tile(f.label, f.value, f.note.filter(|n| !n.is_empty()), f.warn, s)).width(Length::FillPortion(1)));
                n += 1;
            }
            for _ in n..columns {
                r = r.push(Space::new().width(Length::FillPortion(1)));
            }
            tiles = tiles.push(r);
        }
        let mut page = column![head, graph, tiles].spacing(spacing::MD);
        if let Some(below) = below {
            page = page.push(below);
        }
        page.into()
    }

    fn graph<'a>(&self, series: &'a Series, ceiling: f32, height: f32) -> Element<'a, Message> {
        boxed(canvas(Graph { series, ceiling, line: theme::text(), grid: surface::card_border(), motion: self.motion() }).width(Length::Fill).height(Length::Fixed(height)).into())
    }

    /// A resource that was there and is not now (a drive unplugged).
    fn gone(&self, s: FontScale) -> Element<'_, Message> {
        widgets::hint_text("This isn't there any more.", s).into()
    }
}

/// A graph on the page: no card around it, its grid lines are its frame.
fn boxed(body: Element<'_, Message>) -> Element<'_, Message> {
    container(body).width(Length::Fill).into()
}

/// A legend entry: a swatch, a name and a share.
fn key<'a>(color: Color, label: &'static str, value: String, s: FontScale) -> Element<'a, Message> {
    row![
        container(Space::new()).width(Length::Fixed(s.apply(8.0))).height(Length::Fixed(s.apply(8.0))).style(move |_t: &iced::Theme| container::Style {
            background: Some(Background::Color(color)),
            border: Border { radius: 2.0.into(), ..Border::default() },
            ..container::Style::default()
        }),
        widgets::meta_text(label, density::META_TEXT_BASE, s),
        widgets::scaled_text(value, density::META_TEXT_BASE, s).font(theme::mono_font()),
    ]
    .spacing(spacing::XS)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_in_its_two_largest_units() {
        assert_eq!(uptime(41 * 3600 + 27 * 60 + 29), "1 d 17 h");
        assert_eq!(uptime(5 * 3600 + 12 * 60), "5 h 12 m");
        assert_eq!(uptime(14 * 60 + 5), "14 m");
    }

    #[test]
    fn a_gpu_is_named_by_its_model_without_the_maker_it_repeats() {
        assert_eq!(gpu_name("NVIDIA", Some("NVIDIA GeForce RTX 5070 Laptop GPU")), "GeForce RTX 5070");
        assert_eq!(gpu_name("AMD", None), "AMD graphics");
    }
}
