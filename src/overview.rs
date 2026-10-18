//! The Overview: where the window opens — everything at once, laid out
//! the way btop does with atop's detail: the processor by kind and core,
//! memory by use, every GPU, every interface, storage's reads and writes,
//! and the busiest processes with a minute of history each. Pressure is
//! said in the card it is about ("waited 1%"), not a card of its own.
//!
//! Colours come from the theme's roles only. The accent is never one of
//! them: it means selected, and nothing here is.

use crate::details::duration;
use crate::view::{bytes, percent, rate};
use crate::{App, Message};
use hyprforge_procman::fit::Fit;
use hyprforge_procman::graph::Graph;
use hyprforge_procman::model;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{canvas, column, container, row, scrollable, text, Space};
use iced::{Alignment, Background, Border, Color, Element, Length};

/// A card: a titled box, as btop draws its panes.
fn card<'a>(title: String, body: Element<'a, Message>, s: FontScale) -> Element<'a, Message> {
    container(column![widgets::section_label(&title, s), body].spacing(spacing::SM))
        .padding(spacing::MD)
        .width(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: Border { radius: density::card_radius().into(), width: 1.0, color: surface::card_border() },
            ..container::Style::default()
        })
        .into()
}

/// A label and a mono figure on one line, the Overview's unit of detail.
fn pair<'a>(label: impl Into<String>, value: impl Into<String>, s: FontScale) -> Element<'a, Message> {
    row![
        widgets::meta_text(label.into(), density::META_TEXT_BASE, s).width(Length::Fill),
        widgets::scaled_text(value.into(), density::META_TEXT_BASE, s).font(theme::mono_font()),
    ]
    .spacing(spacing::SM)
    .into()
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

/// `2.9 GHz`, or the MHz figure under a gigahertz.
fn clock(mhz: Option<u32>) -> String {
    match mhz {
        Some(m) if m >= 1000 => format!("{:.1} GHz", f64::from(m) / 1000.0),
        Some(m) => format!("{m} MHz"),
        None => "—".to_string(),
    }
}

/// `4.1k`, for counts a second.
fn count(per_second: f64) -> String {
    if per_second >= 10_000.0 {
        format!("{:.0}k", per_second / 1000.0)
    } else if per_second >= 1000.0 {
        format!("{:.1}k", per_second / 1000.0)
    } else {
        format!("{per_second:.0}")
    }
}

/// A line over the last minute, in a card's width.
fn line_graph<'a>(series: &'a hyprforge_procman::history::Series, ceiling: f32, height: f32, motion: Option<hyprforge_procman::graph::Motion>, s: FontScale) -> Element<'a, Message> {
    canvas(Graph { series, ceiling, line: theme::text(), grid: surface::card_border(), motion }).width(Length::Fill).height(Length::Fixed(s.apply(height))).into()
}

/// A figure big enough to read across the room, and what it is of.
fn figure<'a>(value: String, unit: &'static str, s: FontScale) -> Element<'a, Message> {
    row![
        widgets::scaled_text(value, 22.0, s).font(theme::mono_font()),
        widgets::meta_text(unit, density::META_TEXT_BASE, s),
    ]
    .spacing(spacing::XS)
    .align_y(Alignment::End)
    .into()
}

/// A card's title with something said at its right: the figure a glance
/// wants, or where it was measured.
fn titled<'a>(title: &str, aside: String, s: FontScale) -> Element<'a, Message> {
    row![widgets::section_label(title, s), Space::new().width(Length::Fill), widgets::meta_text(aside, density::META_TEXT_BASE, s).font(theme::mono_font())]
        .align_y(Alignment::Center)
        .into()
}

/// The text colour at `alpha`: how this tab tells data apart without
/// borrowing a state colour, whose whole worth is that it means one
/// thing ("info" is somewhere else, "warning" is look here).
pub(crate) fn shade(alpha: f32) -> Color {
    Color { a: alpha, ..theme::text() }
}

/// A core's cell in the heat grid: its share in the text colour, deeper
/// the busier — and solid, with dark ink, past half. Not the accent: on
/// this tab nothing is selected, and the accent means selected. Not a
/// state colour either: a busy core is not news of any kind.
fn core_cell<'a>(used: f32, clock: String, s: FontScale) -> Element<'a, Message> {
    let hot = used > 0.5;
    let base = theme::text();
    let fill = if hot { base } else { Color { a: 0.05 + used.clamp(0.0, 1.0) * 0.5, ..base } };
    let ink = if hot { theme::surface::root() } else { theme::text() };
    container(
        column![
            widgets::scaled_text(format!("{:.0}%", used * 100.0), density::META_TEXT_BASE, s).font(theme::mono_font()).color(ink),
            widgets::scaled_text(clock, density::META_TEXT_BASE * 0.8, s).font(theme::mono_font()).color(if hot { ink } else { theme::text_dim() }),
        ]
        .spacing(1.0)
        .align_x(Alignment::Center),
    )
    .width(Length::FillPortion(1))
    .padding([spacing::XS, 2.0])
    .align_x(Alignment::Center)
    .style(move |_t: &iced::Theme| container::Style {
        background: Some(Background::Color(fill)),
        border: Border { radius: density::inner_radius().into(), ..Border::default() },
        ..container::Style::default()
    })
    .into()
}

/// One of the quarter-screen glance's tiles: what it is, the figure, a
/// minute of it, and a note — the edge in the warning colour when the
/// tile is the likely answer. Pressing one opens Performance.
fn tile<'a>(label: &'static str, value: String, unit: String, graphs: Vec<Element<'a, Message>>, note: String, warn: bool, s: FontScale) -> Element<'a, Message> {
    let ink = if warn { theme::warning() } else { theme::text_dim() };
    let body = column![
        widgets::section_label(label, s),
        row![widgets::scaled_text(value, 20.0, s).font(theme::mono_font()), widgets::meta_text(unit, density::META_TEXT_BASE, s)].spacing(spacing::XS).align_y(Alignment::End),
        iced::widget::Stack::with_children(graphs).width(Length::Fill).height(Length::Fixed(s.apply(34.0))),
        widgets::clamped_text(note, 1).size(s.apply(density::META_TEXT_BASE)).color(ink),
    ]
    .spacing(spacing::XS);
    iced::widget::button(body)
        .padding(spacing::SM)
        .width(Length::FillPortion(1))
        .on_press(Message::Tab(crate::Tab::Performance))
        .style(move |_t: &iced::Theme, status| {
            let edge = match (warn, status) {
                (true, _) => theme::warning(),
                (false, iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed) => theme::text_dim(),
                _ => surface::card_border(),
            };
            iced::widget::button::Style {
                background: Some(Background::Color(surface::card())),
                text_color: theme::text(),
                border: Border { radius: density::card_radius().into(), width: 1.0, color: edge },
                ..iced::widget::button::Style::default()
            }
        })
        .into()
}

/// A core as a bar on its track, as full as the core is busy — never
/// quite empty, so an idle core still reads as a core.
fn core_bar<'a>(used: f32, s: FontScale) -> Element<'a, Message> {
    let full = ((used.clamp(0.0, 1.0) * 100.0).round() as u16).max(2);
    let fill = if used > 0.5 { theme::text() } else { shade(0.5) };
    let rounded = |color: Color| move |_t: &iced::Theme| container::Style { background: Some(Background::Color(color)), border: Border { radius: 2.0.into(), ..Border::default() }, ..container::Style::default() };
    container(column![Space::new().height(Length::FillPortion(101 - full)), container(Space::new()).width(Length::Fill).height(Length::FillPortion(full)).style(rounded(fill))])
        .width(Length::FillPortion(1))
        .height(Length::Fixed(s.apply(18.0)))
        .style(rounded(surface::card_border()))
        .into()
}

/// A plain box for a card, sized by its share of the row.
fn boxed<'a>(body: Element<'a, Message>, portion: u16) -> Element<'a, Message> {
    container(body)
        .padding(spacing::MD)
        .width(Length::FillPortion(portion))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: Border { radius: density::card_radius().into(), width: 1.0, color: surface::card_border() },
            ..container::Style::default()
        })
        .into()
}

impl App {
    /// Four rows: the processor and memory; the GPUs and the network;
    /// storage across the width; the busiest processes. Drawn to the
    /// 2026-10-17 design ("C"): what was the pressure card now sits in
    /// the cards it is about, and every resource with a history draws it
    /// as a line.
    ///
    /// At half a screen the processor and the GPUs take a row each, memory
    /// sits beside the network, and storage and the busiest processes keep
    /// what fits ([`Fit::Half`]). A quarter is the glance instead.
    pub fn overview_view(&self, s: FontScale) -> Element<'_, Message> {
        let wide = match self.fit() {
            Fit::Quarter => return self.glance(s),
            Fit::Half => false,
            Fit::Full => true,
        };
        let page = if wide {
            column![
                row![self.cpu_card(s), self.memory_card(s)].spacing(spacing::MD),
                row![self.gpus_card(s), self.network_card(s)].spacing(spacing::MD),
                self.storage_card(true, s),
                self.top_card(true, s),
            ]
        } else {
            column![
                self.cpu_card(s),
                // Even halves here: the full layout's five-to-one is for a
                // network card sharing a row with the GPUs.
                row![container(self.memory_card(s)).width(Length::FillPortion(1)), container(self.network_card(s)).width(Length::FillPortion(1))].spacing(spacing::MD),
                self.gpus_card(s),
                self.storage_card(false, s),
                self.top_card(false, s),
            ]
        }
        .spacing(spacing::MD)
        .padding(spacing::MD);
        scrollable(page).height(Length::Fill).into()
    }

    /// The share of the last ten seconds something waited for `pick`,
    /// as the kernel's pressure counts it, or nothing if it doesn't.
    fn waited(&self, pick: impl Fn(&hyprforge_procman::machine::PressureDetail) -> f32) -> String {
        self.machine.pressure_detail.as_ref().map_or(String::new(), |p| format!(" · waited {:.0}%", pick(p)))
    }

    fn cpu_card(&self, s: FontScale) -> Element<'_, Message> {
        let m = &self.machine;
        let o = &self.overview;
        let load = m.load.map_or("—".to_string(), |l| format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2]));
        let up = m.uptime_seconds.map_or("—".to_string(), duration);
        let aside = format!(
            "load {load} · up {up}{}{}",
            m.cpu_celsius.map_or(String::new(), |c| format!(" · {c:.0} °C")),
            self.waited(|p| p.cpu_some.avg10)
        );
        let now = row![
            container(figure(percent(self.totals.cpu.map(|c| c * 100.0)), "now", s)).width(Length::Shrink),
            line_graph(&self.history.cpu, 1.0, 64.0, self.motion(), s),
        ]
        .spacing(spacing::MD)
        .align_y(Alignment::End);
        // Where the time went, in /proc/stat's kinds; idle is the track.
        let (user, nice, system, iowait, irq, softirq, steal) = (o.split[0], o.split[1], o.split[2], o.split[4], o.split[5], o.split[6], o.split[7]);
        // Kinds of time are not states, so they are shades of the text
        // colour, each part distinct from the next — save I/O wait, which
        // is the warning colour because waiting on the disk is the thing
        // to look at.
        let colors = [shade(1.0), shade(0.7), shade(0.45), theme::warning(), theme::text_dim(), shade(0.28)];
        let split = widgets::stacked_bar(
            &[(user, colors[0]), (nice, colors[1]), (system, colors[2]), (iowait, colors[3]), (irq + softirq, colors[4]), (steal, colors[5])],
            s,
        );
        let legend = row![
            key(colors[0], "user", percent(Some(user * 100.0)), s),
            key(colors[1], "nice", percent(Some(nice * 100.0)), s),
            key(colors[2], "system", percent(Some(system * 100.0)), s),
            key(colors[3], "I/O wait", percent(Some(iowait * 100.0)), s),
            key(colors[4], "interrupts", percent(Some((irq + softirq) * 100.0)), s),
            key(colors[5], "steal", percent(Some(steal * 100.0)), s),
        ]
        .spacing(spacing::MD);
        let counters = self.last_counters();
        let activity = format!(
            "{} switches/s · {} interrupts/s · {} new/s · {} running · {} blocked on I/O",
            count(o.context_switches),
            count(o.interrupts),
            count(o.forks),
            counters.running,
            counters.blocked
        );
        // Every core as a cell, eight to a row: the busy one stands out
        // without reading a single number.
        let mut grid = column![].spacing(spacing::XS);
        for (n, chunk) in self.history.cores.chunks(8).enumerate() {
            let mut r = row![].spacing(spacing::XS);
            for (i, core) in chunk.iter().enumerate() {
                let mhz = m.clocks.get(n * 8 + i).and_then(|c| c.0);
                r = r.push(core_cell(core.last().unwrap_or(0.0), clock(mhz), s));
            }
            for _ in chunk.len()..8 {
                r = r.push(Space::new().width(Length::FillPortion(1)));
            }
            grid = grid.push(r);
        }
        let body = column![titled("Processor", aside, s), now, grid, split, legend, widgets::meta_text(activity, density::META_TEXT_BASE, s)].spacing(spacing::SM);
        boxed(body.into(), 2)
    }

    fn memory_card(&self, s: FontScale) -> Element<'_, Message> {
        let d = self.machine.memory;
        let t = self.totals;
        let total = t.memory_total.max(1) as f32;
        let f = |b: u64| b as f32 / total;
        // Kinds of memory, in shades of the text colour: none of them is
        // good, bad or elsewhere.
        let colors = [shade(1.0), shade(0.45), shade(0.28), shade(0.7), theme::text_dim()];
        let cache = d.cached.saturating_sub(d.shmem);
        let bar = widgets::stacked_bar(
            &[(f(d.anon), colors[0]), (f(cache), colors[1]), (f(d.buffers), colors[2]), (f(d.shmem), colors[3]), (f(d.slab), colors[4])],
            s,
        );
        let o = &self.overview;
        let body = column![
            titled("Memory", self.waited(|p| p.memory_some.avg10).trim_start_matches(" · ").to_string(), s),
            figure(bytes(t.memory_used), "used", s),
            widgets::meta_text(format!("of {}", bytes(t.memory_total)), density::META_TEXT_BASE, s),
            // Scaled to the whole of memory, so the line's height is how
            // full it is, not how much it wobbled.
            line_graph(&self.history.memory, 1.0, 48.0, self.motion(), s),
            bar,
            key(colors[0], "programs", bytes(d.anon), s),
            key(colors[1], "file cache", bytes(cache), s),
            key(colors[3], "shared", bytes(d.shmem), s),
            key(colors[4], "kernel slab", format!("{} ({} reclaimable)", bytes(d.slab), bytes(d.slab_reclaimable)), s),
            widgets::divider(),
            pair("Available", bytes(t.memory_total.saturating_sub(t.memory_used)), s),
            pair("Swap", format!("{} of {}", bytes(t.swap_used), bytes(t.swap_total)), s),
            pair("Swapping in · out", format!("{} · {}", rate(Some(o.swap_in)), rate(Some(o.swap_out))), s),
        ]
        .spacing(spacing::XS);
        boxed(body.into(), 1)
    }

    /// Every GPU side by side: how busy over the last minute, its video
    /// memory, temperature and power — read as the Performance page reads
    /// them, so an NVIDIA card asleep is said to be, never woken.
    fn gpus_card(&self, s: FontScale) -> Element<'_, Message> {
        let mut cards = row![].spacing(spacing::LG);
        for g in &self.machine.gpus {
            let asleep = g.power.as_deref() == Some("suspended");
            let reading = (g.driver == "nvidia").then_some(self.nvidia.as_ref()).flatten().and_then(|n| n.as_ref().ok());
            // `maker` takes the PCI address, as the process table names cards.
            let maker = hyprforge_procman::gpu::maker(&g.pci, &self.machine.gpus).to_string();
            let name = reading.map_or(format!("{maker} · {}", g.card), |r| r.name.clone());
            let busy = if asleep { "asleep".to_string() } else { percent(self.gpu_busy(g)) };
            let mut col = column![widgets::scaled_text(name, density::ROW_TEXT_BASE, s), figure(busy, if asleep { "" } else { "busy" }, s)].spacing(spacing::XS);
            if let Some(series) = self.history.gpus.get(&g.card) {
                col = col.push(line_graph(series, 1.0, 48.0, self.motion(), s));
            }
            let (vram, celsius, watts) = match reading {
                Some(r) => (r.vram_used.zip(r.vram_total), r.celsius, r.watts),
                None => (g.vram_used.zip(g.vram_total), g.celsius, g.watts),
            };
            if let Some((used, total)) = vram {
                col = col.push(pair("Video memory", format!("{} of {}", bytes(used), bytes(total)), s));
            }
            if !asleep {
                if let Some(c) = celsius {
                    col = col.push(pair("Temperature", format!("{c:.0} °C"), s));
                }
            }
            if let Some(w) = watts {
                col = col.push(pair("Power", format!("{w:.1} W"), s));
            }
            cards = cards.push(container(col).width(Length::FillPortion(1)));
        }
        if self.machine.gpus.is_empty() {
            cards = cards.push(widgets::hint_text("No GPU this window can read.", s));
        }
        boxed(column![widgets::section_label("GPUs", s), cards].spacing(spacing::SM).into(), 7)
    }

    /// What moved, as two lines — reads and writes, every disk together —
    /// then each disk and each filesystem's free space — side by side when
    /// `wide`, one above the other when not.
    fn storage_card(&self, wide: bool, s: FontScale) -> Element<'_, Message> {
        let ceiling = self.history.reads.ceiling(64.0 * 1024.0).max(self.history.writes.ceiling(64.0 * 1024.0));
        let reads = self.history.reads.last().unwrap_or(0.0);
        let writes = self.history.writes.last().unwrap_or(0.0);
        let graph = iced::widget::stack![
            canvas(Graph { series: &self.history.writes, ceiling, line: theme::text(), grid: surface::card_border(), motion: self.motion() }).width(Length::Fill).height(Length::Fixed(s.apply(72.0))),
            canvas(Graph { series: &self.history.reads, ceiling, line: theme::text_dim(), grid: Color::TRANSPARENT, motion: self.motion() }).width(Length::Fill).height(Length::Fixed(s.apply(72.0))),
        ];
        let legend = row![
            key(theme::text_dim(), "read", rate(Some(f64::from(reads))), s),
            key(theme::text(), "write", rate(Some(f64::from(writes))), s),
        ]
        .spacing(spacing::MD);
        let mut disks = column![].spacing(spacing::XS);
        for d in &self.machine.disks {
            let r = self.disk_rates.iter().find(|(n, _)| *n == d.name).map(|(_, r)| *r).unwrap_or_default();
            let name = d.model.clone().unwrap_or_else(|| d.name.clone());
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::META_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            disks = disks.push(
                row![
                    widgets::scaled_text(name, density::META_TEXT_BASE, s).width(Length::Fill),
                    mono(rate(Some(r.read)), 84.0),
                    mono(rate(Some(r.write)), 84.0),
                    mono(d.celsius.map_or("—".to_string(), |c| format!("{c:.0}°")), 44.0),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
            );
        }
        let mut mounts = column![].spacing(spacing::SM);
        for f in &self.machine.filesystems {
            let used = f.total.saturating_sub(f.available);
            let share = if f.total == 0 { 0.0 } else { used as f32 / f.total as f32 };
            mounts = mounts.push(
                column![
                    row![
                        widgets::scaled_text(f.mount.clone(), density::META_TEXT_BASE, s).width(Length::Fill),
                        widgets::meta_text(format!("{} free of {}", bytes(f.available), bytes(f.total)), density::META_TEXT_BASE, s),
                    ],
                    widgets::progress_line(share, s),
                ]
                .spacing(2.0),
            );
        }
        let body = column![
            widgets::section_label("Storage", s),
            legend,
            graph,
            if wide {
                Element::from(row![container(disks).width(Length::FillPortion(1)), container(mounts).width(Length::FillPortion(1))].spacing(spacing::LG))
            } else {
                column![disks, mounts].spacing(spacing::MD).into()
            },
        ]
        .spacing(spacing::SM);
        boxed(body.into(), 1)
    }

    fn network_card(&self, s: FontScale) -> Element<'_, Message> {
        let mut body = column![].spacing(spacing::SM);
        for n in &self.machine.nets {
            let r = self.net_rates.iter().find(|(name, _)| *name == n.name).map(|(_, r)| *r).unwrap_or_default();
            let series = self.history.nets.get(&n.name);
            let mut block = column![widgets::scaled_text(n.name.clone(), density::META_TEXT_BASE, s)].spacing(2.0);
            if let Some(series) = series {
                block = block.push(
                    canvas(Graph { series, ceiling: series.ceiling(64.0 * 1024.0), line: theme::text(), grid: surface::card_border(), motion: self.motion() })
                        .width(Length::Fill)
                        .height(Length::Fixed(s.apply(40.0))),
                );
            }
            block = block
                .push(pair("Down · up", format!("{} · {}", rate(Some(r.rx)), rate(Some(r.tx))), s))
                .push(pair("Packets", format!("{}/s in · {}/s out", count(r.rx_packets), count(r.tx_packets)), s));
            let trouble = format!("{} errors · {} dropped this second · {} · {} since up", r.errors, r.drops, n.errors, n.drops);
            let color = if r.errors + r.drops > 0 { theme::error() } else { theme::text_dim() };
            block = block.push(text(trouble).size(s.apply(density::META_TEXT_BASE)).color(color));
            body = body.push(block);
        }
        container(card("Network".to_string(), body.into(), s)).width(Length::FillPortion(5)).into()
    }

    /// The busiest processes, with a minute of each. Narrow, five
    /// columns of the ten — name, the minute, CPU, memory and PID — and
    /// eight rows: the rest are Processes' and Details' to show.
    fn top_card(&self, wide: bool, s: FontScale) -> Element<'_, Message> {
        let mut busiest: Vec<&model::Entry> = model::entries(&self.table).filter(|e| !e.kernel).collect();
        busiest.sort_by(|a, b| b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)).then(b.private_bytes.cmp(&a.private_bytes)));
        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let minute = if wide { 130.0 } else { 90.0 };
        let mut heads = row![widgets::meta_text("Process", density::META_TEXT_BASE, s).width(Length::Fill), head("Last minute", minute), head("CPU", 64.0)].spacing(spacing::SM);
        if wide {
            heads = heads.push(head("user · sys", 100.0));
        }
        heads = heads.push(head("Memory", 84.0));
        if wide {
            heads = heads.push(head("Swap", 70.0)).push(head("Faults/s", 90.0)).push(head("Switches/s", 110.0)).push(head("Core", 50.0));
        }
        let mut body = column![heads.push(head("PID", 80.0))].spacing(spacing::XS);
        for e in busiest.into_iter().take(if wide { 14 } else { 8 }) {
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::META_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            let d = e.detail.unwrap_or_default();
            let spark: Element<'_, Message> = match self.process_history.get(&e.id) {
                Some(series) => {
                    let ceiling = series.values().fold(0.02f32, f32::max);
                    canvas(Graph { series, ceiling, line: theme::text(), grid: Color::TRANSPARENT, motion: None }).width(Length::Fixed(s.apply(minute))).height(Length::Fixed(s.apply(18.0))).into()
                }
                None => Space::new().width(Length::Fixed(s.apply(minute))).into(),
            };
            let mut line = row![widgets::scaled_text(e.name.clone(), density::META_TEXT_BASE, s).width(Length::Fill), spark, mono(percent(e.cpu), 64.0)].spacing(spacing::SM).align_y(Alignment::Center);
            if wide {
                line = line.push(mono(format!("{:.1} · {:.1}", d.user_pct, d.system_pct), 100.0));
            }
            line = line.push(mono(bytes(e.private_bytes), 84.0));
            if wide {
                let switches = match (d.voluntary, d.forced) {
                    (Some(v), Some(f)) => format!("{} · {}", count(v), count(f)),
                    _ => "—".to_string(),
                };
                let core = if e.processor >= 0 { e.processor.to_string() } else { "—".to_string() };
                line = line
                    .push(mono(e.swap_bytes.map_or("—".to_string(), bytes), 70.0))
                    .push(mono(format!("{} · {}", count(d.minor_faults), count(d.major_faults)), 90.0))
                    .push(mono(switches, 110.0))
                    .push(mono(core, 50.0));
            }
            body = body.push(line.push(mono(e.id.pid.to_string(), 80.0)));
        }
        card("Busiest processes".to_string(), body.into(), s)
    }

    /// A quarter of a screen: four tiles that each answer "is this the
    /// problem?", every core as a bar, and the five processes most likely
    /// to be it. Nothing scrolls; a tile opens Performance, where the rest
    /// of it is.
    fn glance(&self, s: FontScale) -> Element<'_, Message> {
        let m = &self.machine;
        let t = self.totals;
        let motion = self.motion();
        let line = |series, ceiling, color| -> Element<'_, Message> { canvas(Graph { series, ceiling, line: color, grid: Color::TRANSPARENT, motion }).width(Length::Fill).height(Length::Fill).into() };

        let cpu = tile(
            "CPU",
            percent(t.cpu.map(|c| c * 100.0)),
            m.cpu_celsius.map_or(String::new(), |c| format!("{c:.0} °C")),
            vec![line(&self.history.cpu, 1.0, theme::text())],
            m.load.map_or(String::new(), |l| format!("load {:.2}", l[0])),
            false,
            s,
        );
        let memory = tile(
            "Memory",
            bytes(t.memory_used),
            format!("of {}", bytes(t.memory_total)),
            vec![line(&self.history.memory, 1.0, theme::text())],
            format!("{} available", bytes(t.memory_total.saturating_sub(t.memory_used))),
            false,
            s,
        );
        // The discrete card leads — on a laptop it is the one whose being
        // busy is news — and every other is the second line and the note.
        let mut gpus: Vec<&hyprforge_procman::machine::Gpu> = m.gpus.iter().collect();
        gpus.sort_by_key(|g| g.driver != "nvidia");
        let busy = |g: &hyprforge_procman::machine::Gpu| if g.power.as_deref() == Some("suspended") { "asleep".to_string() } else { percent(self.gpu_busy(g)) };
        let maker = |g: &hyprforge_procman::machine::Gpu| hyprforge_procman::gpu::maker(&g.pci, &m.gpus).to_string();
        let gpu = match gpus.split_first() {
            Some((lead, rest)) => {
                let mut graphs = Vec::new();
                for (g, color) in rest.iter().take(1).map(|g| (g, theme::text_dim())).chain([(lead, theme::text())]) {
                    if let Some(series) = self.history.gpus.get(&g.card) {
                        graphs.push(line(series, 1.0, color));
                    }
                }
                let note = rest.iter().map(|g| format!("{} {}", maker(g), busy(g))).collect::<Vec<_>>().join(" · ");
                tile("GPU", busy(lead), maker(lead), graphs, note, false, s)
            }
            None => tile("GPU", "—".to_string(), String::new(), Vec::new(), "No GPU this window can read".to_string(), false, s),
        };
        let ceiling = self.history.reads.ceiling(64.0 * 1024.0).max(self.history.writes.ceiling(64.0 * 1024.0));
        let iowait = self.overview.split[4];
        let disk = tile(
            "Disk",
            rate(Some(f64::from(self.history.writes.last().unwrap_or(0.0)))),
            "write".to_string(),
            vec![line(&self.history.reads, ceiling, theme::text_dim()), line(&self.history.writes, ceiling, theme::text())],
            format!("read {} · I/O wait {}", rate(Some(f64::from(self.history.reads.last().unwrap_or(0.0)))), percent(Some(iowait * 100.0))),
            // Waiting on the disk is the thing at this size most likely to
            // be why the machine feels slow, and the one a number alone
            // does not flag.
            iowait > 0.1,
            s,
        );

        // Sixteen bars and no numbers: at this size the question is "is
        // one core pinned", and a bar answers it faster than a figure.
        let mut cores = row![widgets::section_label("Cores", s)].spacing(2.0).align_y(Alignment::Center);
        cores = cores.push(Space::new().width(Length::Fixed(spacing::SM)));
        for core in &self.history.cores {
            cores = cores.push(core_bar(core.last().unwrap_or(0.0), s));
        }

        let mut busiest: Vec<&model::Entry> = model::entries(&self.table).filter(|e| !e.kernel).collect();
        busiest.sort_by(|a, b| b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)).then(b.private_bytes.cmp(&a.private_bytes)));
        let mut list = column![row![widgets::section_label("Busiest", s), Space::new().width(Length::Fill)]].spacing(spacing::XS);
        for e in busiest.into_iter().take(5) {
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::META_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            let spark: Element<'_, Message> = match self.process_history.get(&e.id) {
                Some(series) => {
                    let ceiling = series.values().fold(0.02f32, f32::max);
                    canvas(Graph { series, ceiling, line: theme::text(), grid: Color::TRANSPARENT, motion: None }).width(Length::Fixed(s.apply(80.0))).height(Length::Fixed(s.apply(16.0))).into()
                }
                None => Space::new().width(Length::Fixed(s.apply(80.0))).into(),
            };
            list = list.push(
                row![widgets::scaled_text(e.name.clone(), density::META_TEXT_BASE, s).width(Length::Fill), spark, mono(percent(e.cpu), 64.0), mono(bytes(e.private_bytes), 84.0)]
                    .spacing(spacing::SM)
                    .align_y(Alignment::Center),
            );
        }
        column![
            row![cpu, memory, gpu, disk].spacing(spacing::SM),
            boxed(column![cores, widgets::divider(), list].spacing(spacing::SM).into(), 1),
        ]
        .spacing(spacing::SM)
        .padding(spacing::SM)
        .height(Length::Fill)
        .into()
    }

    /// The last reading's running and blocked counts.
    fn last_counters(&self) -> hyprforge_procman::proc::Counters {
        self.counters
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_clocks_read_compactly() {
        assert_eq!(count(4123.0), "4.1k");
        assert_eq!(count(41230.0), "41k");
        assert_eq!(count(12.4), "12");
        assert_eq!(clock(Some(2875)), "2.9 GHz");
        assert_eq!(clock(Some(400)), "400 MHz");
        assert_eq!(clock(None), "—");
    }
}
