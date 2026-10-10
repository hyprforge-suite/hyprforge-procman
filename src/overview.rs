//! The Overview: the Advanced view's dashboard, everything at once the
//! way btop lays it out, with atop's verbosity — CPU by kind and core,
//! memory by use, every disk's latency and queue, every interface's
//! errors, pressure over three windows, and the busiest processes with a
//! minute of history each.
//!
//! Colours come from the theme's roles only. The accent is never one of
//! them: it means selected, and nothing here is.

use crate::details::duration;
use crate::view::{bytes, percent, rate};
use crate::{App, Message};
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

impl App {
    pub fn overview_view(&self, s: FontScale) -> Element<'_, Message> {
        let page = column![
            row![self.cpu_card(s), self.memory_card(s)].spacing(spacing::MD),
            row![self.disks_card(s), self.network_card(s), self.pressure_card(s)].spacing(spacing::MD),
            self.top_card(s),
        ]
        .spacing(spacing::MD)
        .padding(spacing::MD);
        scrollable(page).height(Length::Fill).into()
    }

    fn cpu_card(&self, s: FontScale) -> Element<'_, Message> {
        let m = &self.machine;
        let o = &self.overview;
        let load = m.load.map_or("—".to_string(), |l| format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2]));
        let up = m.uptime_seconds.map_or("—".to_string(), duration);
        let headline = format!(
            "CPU {} · load {load} · up {up}{}",
            percent(self.totals.cpu.map(|c| c * 100.0)),
            m.cpu_celsius.map_or(String::new(), |c| format!(" · {c:.0} °C"))
        );
        // Where the time went, in /proc/stat's kinds; idle is the track.
        let (user, nice, system, iowait, irq, softirq, steal) = (o.split[0], o.split[1], o.split[2], o.split[4], o.split[5], o.split[6], o.split[7]);
        let colors = [theme::info(), theme::success(), theme::warning(), theme::error(), theme::text_dim(), theme::text()];
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
        // Each core: its use, its clock now, a thin bar.
        let mut cores = column![].spacing(spacing::XS);
        let per_row = 4;
        for (n, chunk) in self.history.cores.chunks(per_row).enumerate() {
            let mut r = row![].spacing(spacing::MD);
            for (i, core) in chunk.iter().enumerate() {
                let index = n * per_row + i;
                let used = core.last().unwrap_or(0.0);
                let mhz = m.clocks.get(index).and_then(|c| c.0);
                r = r.push(
                    column![
                        row![
                            widgets::meta_text(format!("{index}"), density::META_TEXT_BASE, s).width(Length::Fixed(s.apply(22.0))),
                            widgets::scaled_text(percent(Some(used * 100.0)), density::META_TEXT_BASE, s).font(theme::mono_font()).width(Length::Fill),
                            widgets::meta_text(clock(mhz), density::META_TEXT_BASE, s).font(theme::mono_font()),
                        ],
                        widgets::progress_line(used, s),
                    ]
                    .spacing(2.0)
                    .width(Length::FillPortion(1)),
                );
            }
            cores = cores.push(r);
        }
        let graph = container(canvas(Graph { series: &self.history.cpu, ceiling: 1.0, line: theme::text(), grid: surface::card_border() }).width(Length::Fill).height(Length::Fixed(s.apply(90.0))));
        let body = column![
            widgets::scaled_text(headline, density::ROW_TEXT_BASE, s),
            graph,
            split,
            legend,
            widgets::meta_text(activity, density::META_TEXT_BASE, s),
            cores,
        ]
        .spacing(spacing::SM);
        container(card("Processor".to_string(), body.into(), s)).width(Length::FillPortion(2)).into()
    }

    fn memory_card(&self, s: FontScale) -> Element<'_, Message> {
        let d = self.machine.memory;
        let t = self.totals;
        let total = t.memory_total.max(1) as f32;
        let f = |b: u64| b as f32 / total;
        let colors = [theme::info(), theme::success(), theme::text_dim(), theme::warning(), theme::error()];
        let cache = d.cached.saturating_sub(d.shmem);
        let bar = widgets::stacked_bar(
            &[(f(d.anon), colors[0]), (f(cache), colors[1]), (f(d.buffers), colors[2]), (f(d.shmem), colors[3]), (f(d.slab), colors[4])],
            s,
        );
        let o = &self.overview;
        let body = column![
            widgets::scaled_text(format!("{} used of {}", bytes(t.memory_used), bytes(t.memory_total)), density::ROW_TEXT_BASE, s),
            bar,
            key(colors[0], "programs", bytes(d.anon), s),
            key(colors[1], "file cache", bytes(cache), s),
            key(colors[2], "buffers", bytes(d.buffers), s),
            key(colors[3], "shared", bytes(d.shmem), s),
            key(colors[4], "kernel slab", format!("{} ({} reclaimable)", bytes(d.slab), bytes(d.slab_reclaimable)), s),
            widgets::divider(),
            pair("Available", bytes(t.memory_total.saturating_sub(t.memory_used)), s),
            pair("Free", bytes(d.free), s),
            pair("Dirty · writing back", format!("{} · {}", bytes(d.dirty), bytes(d.writeback)), s),
            pair("Page tables · kernel stacks", format!("{} · {}", bytes(d.page_tables), bytes(d.kernel_stack)), s),
            pair("Swap", format!("{} of {}", bytes(t.swap_used), bytes(t.swap_total)), s),
            pair("Swapping in · out", format!("{} · {}", rate(Some(o.swap_in)), rate(Some(o.swap_out))), s),
            pair("Page faults · from disk", format!("{}/s · {}/s", count(o.faults), count(o.major_faults)), s),
        ]
        .spacing(spacing::XS);
        container(card("Memory".to_string(), body.into(), s)).width(Length::FillPortion(1)).into()
    }

    fn disks_card(&self, s: FontScale) -> Element<'_, Message> {
        let mut body = column![].spacing(spacing::SM);
        for d in &self.machine.disks {
            let r = self.disk_rates.iter().find(|(n, _)| *n == d.name).map(|(_, r)| *r).unwrap_or_default();
            let name = format!("{}{}", d.name, d.model.as_ref().map_or(String::new(), |m| format!(" · {m}")));
            body = body.push(
                column![
                    row![
                        widgets::scaled_text(name, density::META_TEXT_BASE, s).width(Length::Fill),
                        widgets::meta_text(d.celsius.map_or(String::new(), |c| format!("{c:.0} °C")), density::META_TEXT_BASE, s),
                    ],
                    widgets::progress_line(r.active, s),
                    pair("Read · write", format!("{} · {}", rate(Some(r.read)), rate(Some(r.write))), s),
                    pair("Operations", format!("{} r/s · {} w/s", count(r.read_ops), count(r.write_ops)), s),
                    pair("Average wait · queue", format!("{} · {:.2}", r.wait_ms.map_or("—".to_string(), |w| format!("{w:.1} ms")), r.queue), s),
                ]
                .spacing(2.0),
            );
        }
        body = body.push(widgets::divider());
        for f in &self.machine.filesystems {
            let used = f.total.saturating_sub(f.available);
            let share = if f.total == 0 { 0.0 } else { used as f32 / f.total as f32 };
            body = body.push(
                column![
                    row![
                        widgets::scaled_text(f.mount.clone(), density::META_TEXT_BASE, s).width(Length::Fill),
                        widgets::meta_text(format!("{} free of {} · {}", bytes(f.available), bytes(f.total), f.kind), density::META_TEXT_BASE, s),
                    ],
                    widgets::progress_line(share, s),
                ]
                .spacing(2.0),
            );
        }
        container(card("Storage".to_string(), body.into(), s)).width(Length::FillPortion(1)).into()
    }

    fn network_card(&self, s: FontScale) -> Element<'_, Message> {
        let mut body = column![].spacing(spacing::SM);
        for n in &self.machine.nets {
            let r = self.net_rates.iter().find(|(name, _)| *name == n.name).map(|(_, r)| *r).unwrap_or_default();
            let series = self.history.nets.get(&n.name);
            let mut block = column![widgets::scaled_text(n.name.clone(), density::META_TEXT_BASE, s)].spacing(2.0);
            if let Some(series) = series {
                block = block.push(
                    canvas(Graph { series, ceiling: series.ceiling(64.0 * 1024.0), line: theme::text(), grid: surface::card_border() })
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
        container(card("Network".to_string(), body.into(), s)).width(Length::FillPortion(1)).into()
    }

    fn pressure_card(&self, s: FontScale) -> Element<'_, Message> {
        let Some(p) = self.machine.pressure_detail else {
            return container(card("Pressure".to_string(), widgets::hint_text("This kernel doesn't report pressure.", s).into(), s)).width(Length::FillPortion(1)).into();
        };
        let line = |label: &'static str, w: hyprforge_procman::machine::Windows| -> Element<'_, Message> {
            column![
                row![
                    widgets::meta_text(label, density::META_TEXT_BASE, s).width(Length::Fill),
                    widgets::scaled_text(format!("{:>5.1} {:>5.1} {:>5.1}", w.avg10, w.avg60, w.avg300), density::META_TEXT_BASE, s).font(theme::mono_font()),
                ],
                widgets::progress_line(w.avg10 / 100.0, s),
            ]
            .spacing(2.0)
            .into()
        };
        let body = column![
            widgets::meta_text("Share of time something waited — 10 s, 1 min, 5 min", density::META_TEXT_BASE, s),
            line("CPU, some waited", p.cpu_some),
            line("Memory, some", p.memory_some),
            line("Memory, everything", p.memory_full),
            line("Storage, some", p.io_some),
            line("Storage, everything", p.io_full),
        ]
        .spacing(spacing::SM);
        container(card("Pressure".to_string(), body.into(), s)).width(Length::FillPortion(1)).into()
    }

    fn top_card(&self, s: FontScale) -> Element<'_, Message> {
        let mut busiest: Vec<&model::Entry> = model::entries(&self.table).filter(|e| !e.kernel).collect();
        busiest.sort_by(|a, b| b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)).then(b.private_bytes.cmp(&a.private_bytes)));
        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let mut body = column![row![
            widgets::meta_text("Process", density::META_TEXT_BASE, s).width(Length::Fill),
            head("Last minute", 130.0),
            head("CPU", 64.0),
            head("user · sys", 100.0),
            head("Memory", 84.0),
            head("Swap", 70.0),
            head("Faults/s", 90.0),
            head("Switches/s", 110.0),
            head("Core", 50.0),
            head("PID", 80.0),
        ]
        .spacing(spacing::SM)]
        .spacing(spacing::XS);
        for e in busiest.into_iter().take(14) {
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::META_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            let d = e.detail.unwrap_or_default();
            let spark: Element<'_, Message> = match self.process_history.get(&e.id) {
                Some(series) => {
                    let ceiling = series.values().fold(0.02f32, f32::max);
                    canvas(Graph { series, ceiling, line: theme::text(), grid: Color::TRANSPARENT }).width(Length::Fixed(s.apply(130.0))).height(Length::Fixed(s.apply(18.0))).into()
                }
                None => Space::new().width(Length::Fixed(s.apply(130.0))).into(),
            };
            let switches = match (d.voluntary, d.forced) {
                (Some(v), Some(f)) => format!("{} · {}", count(v), count(f)),
                _ => "—".to_string(),
            };
            let core = if e.processor >= 0 { e.processor.to_string() } else { "—".to_string() };
            body = body.push(
                row![
                    widgets::scaled_text(e.name.clone(), density::META_TEXT_BASE, s).width(Length::Fill),
                    spark,
                    mono(percent(e.cpu), 64.0),
                    mono(format!("{:.1} · {:.1}", d.user_pct, d.system_pct), 100.0),
                    mono(bytes(e.private_bytes), 84.0),
                    mono(e.swap_bytes.map_or("—".to_string(), bytes), 70.0),
                    mono(format!("{} · {}", count(d.minor_faults), count(d.major_faults)), 90.0),
                    mono(switches, 110.0),
                    mono(core, 50.0),
                    mono(e.id.pid.to_string(), 80.0),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
            );
        }
        card("Busiest processes".to_string(), body.into(), s)
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
