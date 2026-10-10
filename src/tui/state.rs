//! What the TUI is showing and what a key does — no terminal, no I/O, so
//! every key is tested. atop's keys where atop has one, so its users'
//! hands already know them.

use crate::moment::{Moment, Row};

/// Which columns the process list shows — atop's view keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// `g`: CPU, memory, disk, state.
    Generic,
    /// `m`: virtual, resident and proportional memory, growth, swap, faults.
    Memory,
    /// `d`: reads, writes, cancelled writes.
    Disk,
    /// `n`: network received and sent, from the eBPF counter (root).
    Network,
    /// `s`: threads by state, policy, nice, priority, CPU, delays.
    Scheduling,
    /// `v`: parent, start time, owner.
    Various,
    /// `c`: the whole command line.
    Command,
    /// `u`: everything a user's processes did, added up.
    Users,
    /// `p`: everything a program's processes did, added up.
    Programs,
    /// `j`: every cgroup — each service, slice and scope.
    Cgroups,
    /// `y`: every thread, by its own name (live only).
    Threads,
    /// `e`: each process holding a GPU — its busiest engine and its video
    /// memory (live only).
    Gpu,
}

impl View {
    /// Read from `/proc` as the screen is drawn, not kept in the journal.
    pub fn live_only(self) -> bool {
        matches!(self, View::Threads | View::Gpu)
    }

    /// Lists something other than processes, so nothing in it is a
    /// process to end.
    pub fn lists_processes(self) -> bool {
        !matches!(self, View::Users | View::Programs | View::Cgroups | View::Threads | View::Gpu)
    }
}

/// atop's sort keys: `C` CPU, `M` memory, `D` disk, `N` network, `A`
/// whichever the machine is busiest with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Memory,
    Disk,
    Network,
    Auto,
}

/// A question the bottom line is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// `/`: the filter being typed.
    Filter(String),
    /// `b`: a time to go to, `HH:MM`.
    Branch(String),
    /// `i`: a new interval, seconds.
    Interval(String),
    /// `k`: ending this process, confirm with `y`.
    Kill { pid: i32, start: u64, name: String },
}

/// What the loop around the state must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Replay: move by this many samples.
    Step(i64),
    /// Replay: go to the first sample at or after this time of day.
    Branch(chrono::NaiveTime),
    /// Replay: back to the first sample.
    Rewind,
    Interval(u32),
    /// End this process (live only, after the confirmation).
    Kill { pid: i32, start: u64 },
}

/// A key, as the state reads it — the terminal's event, already reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
}

#[derive(Debug, Clone)]
pub struct State {
    pub view: View,
    pub sort: SortKey,
    pub filter: String,
    pub selected: usize,
    pub paused: bool,
    pub prompt: Option<Prompt>,
    /// Replaying a journal (time keys work; kill does not).
    pub replay: bool,
    /// A line to show once, then clear.
    pub message: Option<String>,
    pub help: bool,
}

impl State {
    pub fn new(replay: bool) -> State {
        State { view: View::Generic, sort: SortKey::Auto, filter: String::new(), selected: 0, paused: false, prompt: None, replay, message: None, help: false }
    }

    /// The rows the list shows, in order: filtered, then sorted.
    /// The filter, applied to a name and a pid of something that isn't a
    /// process row (a thread, a GPU user).
    pub fn shows(&self, names: &[&str], pids: &[i32]) -> bool {
        let needle = self.filter.to_lowercase();
        needle.is_empty() || names.iter().any(|n| n.to_lowercase().contains(&needle)) || pids.iter().any(|p| p.to_string() == needle)
    }

    pub fn rows<'m>(&self, moment: &'m Moment) -> Vec<&'m Row> {
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<&Row> = moment
            .rows
            .iter()
            .filter(|r| !r.kernel || r.cpu_pct > 0.0)
            .filter(|r| {
                needle.is_empty()
                    || r.name.to_lowercase().contains(&needle)
                    || r.command.to_lowercase().contains(&needle)
                    || r.user.to_lowercase() == needle
                    || r.id.pid.to_string() == needle
            })
            .collect();
        let key = match self.sort {
            SortKey::Auto => auto_key(moment),
            k => k,
        };
        rows.sort_by(|a, b| match key {
            SortKey::Memory => b.resident_bytes.cmp(&a.resident_bytes),
            SortKey::Disk => (b.read.unwrap_or(0) + b.written.unwrap_or(0)).cmp(&(a.read.unwrap_or(0) + a.written.unwrap_or(0))),
            SortKey::Network => (b.net_rx.unwrap_or(0) + b.net_tx.unwrap_or(0)).cmp(&(a.net_rx.unwrap_or(0) + a.net_tx.unwrap_or(0))),
            _ => b.cpu_pct.total_cmp(&a.cpu_pct),
        }
        .then(a.id.pid.cmp(&b.id.pid)));
        rows
    }

    /// One key. `selected_row` is the process under the cursor, for `k`.
    pub fn key(&mut self, key: Key, selected_row: Option<&Row>) -> Action {
        self.message = None;
        if let Some(prompt) = self.prompt.take() {
            return self.answer(prompt, key);
        }
        if self.help {
            self.help = false;
            return Action::None;
        }
        match key {
            Key::Char('q') => return Action::Quit,
            Key::Char('g') => self.view = View::Generic,
            Key::Char('m') => self.view = View::Memory,
            Key::Char('d') => self.view = View::Disk,
            Key::Char('n') => self.view = View::Network,
            Key::Char('s') => self.view = View::Scheduling,
            Key::Char('v') => self.view = View::Various,
            Key::Char('c') => self.view = View::Command,
            Key::Char('u') => self.view = View::Users,
            Key::Char('p') => self.view = View::Programs,
            Key::Char('j') => self.view = View::Cgroups,
            Key::Char('y') => self.view = View::Threads,
            Key::Char('e') => self.view = View::Gpu,
            Key::Char('C') => self.sort = SortKey::Cpu,
            Key::Char('M') => self.sort = SortKey::Memory,
            Key::Char('D') => self.sort = SortKey::Disk,
            Key::Char('N') => self.sort = SortKey::Network,
            Key::Char('A') => self.sort = SortKey::Auto,
            Key::Char('/') => self.prompt = Some(Prompt::Filter(self.filter.clone())),
            Key::Char('z') if !self.replay => self.paused = !self.paused,
            Key::Char('i') if !self.replay => self.prompt = Some(Prompt::Interval(String::new())),
            Key::Char('t') if self.replay => return Action::Step(1),
            Key::Char('T') if self.replay => return Action::Step(-1),
            Key::Char('b') if self.replay => self.prompt = Some(Prompt::Branch(String::new())),
            Key::Char('r') if self.replay => return Action::Rewind,
            Key::Char('k') => match (self.replay, selected_row) {
                (true, _) => self.message = Some("A replay can't end anything — it already happened.".into()),
                (false, Some(r)) if r.exit.is_none() => {
                    self.prompt = Some(Prompt::Kill { pid: r.id.pid, start: r.id.start, name: r.name.clone() })
                }
                _ => self.message = Some("Choose a running process first.".into()),
            },
            Key::Char('h') | Key::Char('?') => self.help = true,
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected += 1,
            Key::PageUp => self.selected = self.selected.saturating_sub(20),
            Key::PageDown => self.selected += 20,
            Key::Home => self.selected = 0,
            Key::Escape => self.filter.clear(),
            _ => {}
        }
        Action::None
    }

    fn answer(&mut self, prompt: Prompt, key: Key) -> Action {
        let typed = |text: String, this: &mut State, wrap: fn(String) -> Prompt| match key {
            Key::Char(c) => {
                let mut t = text;
                t.push(c);
                this.prompt = Some(wrap(t));
            }
            Key::Backspace => {
                let mut t = text;
                t.pop();
                this.prompt = Some(wrap(t));
            }
            _ => this.prompt = Some(wrap(text)),
        };
        match (prompt, key) {
            (Prompt::Filter(text), Key::Enter) => {
                self.filter = text;
                self.selected = 0;
            }
            (_, Key::Escape) => {}
            (Prompt::Filter(text), _) => typed(text, self, Prompt::Filter),
            (Prompt::Branch(text), Key::Enter) => match parse_time(&text) {
                Some(at) => return Action::Branch(at),
                None => self.message = Some(format!("{text:?} isn't a time — HH:MM")),
            },
            (Prompt::Branch(text), _) => typed(text, self, Prompt::Branch),
            (Prompt::Interval(text), Key::Enter) => match text.trim().parse::<u32>() {
                Ok(n) if n > 0 => return Action::Interval(n),
                _ => self.message = Some(format!("{text:?} isn't a number of seconds")),
            },
            (Prompt::Interval(text), _) => typed(text, self, Prompt::Interval),
            (Prompt::Kill { pid, start, .. }, Key::Char('y')) => return Action::Kill { pid, start },
            (Prompt::Kill { .. }, _) => self.message = Some("Not ended.".into()),
        }
        Action::None
    }
}

/// `HH:MM`, or `HH:MM:SS` — to the second, so a window can be lined up
/// with another tool's (atop's samples end at :37 past the minute here).
pub fn parse_time(text: &str) -> Option<chrono::NaiveTime> {
    let text = text.trim();
    chrono::NaiveTime::parse_from_str(text, "%H:%M:%S").or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M")).ok()
}

/// What `A` sorts by: whichever the machine is spending most on — disk
/// when something is busy with storage, memory when it is short,
/// otherwise CPU. atop's rule, in short.
pub fn auto_key(moment: &Moment) -> SortKey {
    let s = &moment.system;
    let disk_busy = s.disks.iter().map(|d| d.busy_pct).fold(0.0f32, f32::max);
    let cpu_busy = 100.0 - s.cpu[3] - s.cpu[4];
    if disk_busy > 70.0 && disk_busy > cpu_busy {
        SortKey::Disk
    } else if s.scanned > 0 || s.swap_out > 0 {
        SortKey::Memory
    } else {
        SortKey::Cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moment::Ended;
    use crate::sample::Id;

    fn row(pid: i32, name: &str, cpu: f32, mem: u64) -> Row {
        Row { id: Id { pid, start: 1 }, name: name.into(), user: "voldemort".into(), cpu_pct: cpu, resident_bytes: mem, ..Row::default() }
    }

    fn moment() -> Moment {
        Moment { rows: vec![row(1, "init", 0.1, 50), row(2, "blender", 80.0, 900), row(3, "kitty", 2.0, 2000)], ..Moment::default() }
    }

    #[test]
    fn atops_view_and_sort_keys_switch_the_list() {
        let mut s = State::new(false);
        s.key(Key::Char('m'), None);
        assert_eq!(s.view, View::Memory);
        s.key(Key::Char('M'), None);
        let m = moment();
        let names: Vec<&str> = s.rows(&m).iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["kitty", "blender", "init"]);
        s.key(Key::Char('C'), None);
        assert_eq!(s.rows(&moment())[0].name, "blender");
    }

    #[test]
    fn a_filter_is_typed_then_applied_and_escape_clears_it() {
        let mut s = State::new(false);
        for k in [Key::Char('/'), Key::Char('k'), Key::Char('i'), Key::Char('t'), Key::Enter] {
            s.key(k, None);
        }
        assert_eq!(s.filter, "kit");
        assert_eq!(s.rows(&moment()).len(), 1);
        s.key(Key::Escape, None);
        assert!(s.filter.is_empty());
    }

    #[test]
    fn killing_asks_first_and_only_y_ends_it() {
        let mut s = State::new(false);
        let target = row(2, "blender", 80.0, 0);
        assert_eq!(s.key(Key::Char('k'), Some(&target)), Action::None);
        assert!(matches!(s.prompt, Some(Prompt::Kill { pid: 2, .. })));
        assert_eq!(s.key(Key::Char('n'), None), Action::None, "anything but y is no");
        s.key(Key::Char('k'), Some(&target));
        assert_eq!(s.key(Key::Char('y'), None), Action::Kill { pid: 2, start: 1 });
    }

    #[test]
    fn a_replay_moves_through_time_and_never_kills() {
        let mut s = State::new(true);
        assert_eq!(s.key(Key::Char('t'), None), Action::Step(1));
        assert_eq!(s.key(Key::Char('T'), None), Action::Step(-1));
        assert_eq!(s.key(Key::Char('r'), None), Action::Rewind);
        assert_eq!(s.key(Key::Char('k'), Some(&row(2, "x", 0.0, 0))), Action::None);
        assert!(s.message.is_some() && s.prompt.is_none());
        for k in [Key::Char('b'), Key::Char('0'), Key::Char('3'), Key::Char(':'), Key::Char('1'), Key::Char('5')] {
            s.key(k, None);
        }
        assert_eq!(s.key(Key::Enter, None), Action::Branch(chrono::NaiveTime::from_hms_opt(3, 15, 0).unwrap()));
        s.key(Key::Char('b'), None);
        s.key(Key::Char('x'), None);
        assert_eq!(s.key(Key::Enter, None), Action::None);
        assert!(s.message.as_deref().unwrap().contains("HH:MM"));
    }

    #[test]
    fn an_ended_process_cannot_be_killed_again() {
        let mut s = State::new(false);
        let gone = Row { exit: Some(Ended::Code(0)), ..row(9, "sleep", 0.0, 0) };
        s.key(Key::Char('k'), Some(&gone));
        assert!(s.prompt.is_none());
    }

    #[test]
    fn auto_sorts_by_what_the_machine_is_busiest_with() {
        let mut m = moment();
        m.system.cpu = [10.0, 0.0, 5.0, 85.0, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(auto_key(&m), SortKey::Cpu);
        m.system.disks.push(crate::moment::DiskLine { name: "nvme0n1".into(), busy_pct: 95.0, ..Default::default() });
        assert_eq!(auto_key(&m), SortKey::Disk);
    }

    #[test]
    fn nothing_in_a_view_of_threads_gpus_or_totals_is_a_process_to_end() {
        for view in [View::Users, View::Programs, View::Cgroups, View::Threads, View::Gpu] {
            assert!(!view.lists_processes(), "{view:?}");
        }
        assert!(View::Generic.lists_processes() && View::Network.lists_processes());
        assert!(View::Threads.live_only() && View::Gpu.live_only() && !View::Generic.live_only());
    }

    #[test]
    fn a_time_is_read_to_the_minute_or_the_second() {
        let t = |h, m, s| chrono::NaiveTime::from_hms_opt(h, m, s);
        assert_eq!([parse_time("03:15"), parse_time(" 19:06:37 "), parse_time("3:05")], [t(3, 15, 0), t(19, 6, 37), t(3, 5, 0)]);
        assert_eq!([parse_time("25:00"), parse_time("12:60"), parse_time("noon")], [None, None, None]);
    }
}
