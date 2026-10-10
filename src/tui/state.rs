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
}

/// atop's sort keys: `C` CPU, `M` memory, `D` disk, `A` whichever the
/// machine is busiest with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Memory,
    Disk,
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
    Branch { hour: u32, minute: u32 },
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
            Key::Char('s') => self.view = View::Scheduling,
            Key::Char('v') => self.view = View::Various,
            Key::Char('c') => self.view = View::Command,
            Key::Char('u') => self.view = View::Users,
            Key::Char('p') => self.view = View::Programs,
            Key::Char('j') => self.view = View::Cgroups,
            Key::Char('C') => self.sort = SortKey::Cpu,
            Key::Char('M') => self.sort = SortKey::Memory,
            Key::Char('D') => self.sort = SortKey::Disk,
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
                Some((hour, minute)) => return Action::Branch { hour, minute },
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

/// `HH:MM`.
pub fn parse_time(text: &str) -> Option<(u32, u32)> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
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
        assert_eq!(s.key(Key::Enter, None), Action::Branch { hour: 3, minute: 15 });
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
}
