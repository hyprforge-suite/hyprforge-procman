//! The list: apps first, then everything else, sorted and searched.
//!
//! An **app** is a process that owns a window, together with every
//! process it started that does not own a window of its own — a browser
//! and its content processes, a terminal and the shell in it. Its CPU and
//! memory are the group's, because that is what closing the app would
//! give back. Every other process is **background**. A process that owns
//! a window inside another app's tree (something launched from a
//! terminal) is an app of its own, and is not counted twice.
//!
//! Everything here is plain data in, plain data out: the list a frame
//! draws is [`lines`] of a [`Table`], so sorting, searching and grouping
//! are tested without a window, `/proc` or Hyprland.

use crate::proc::{Proc, Snapshot};
use crate::sample::{Id, Use};
use crate::windows::Window;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// One process, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: Id,
    pub name: String,
    pub command: String,
    pub user: String,
    /// The current user's own: something they can end without asking for
    /// administrator access.
    pub own: bool,
    /// Percent of the whole machine; `None` until a second reading.
    pub cpu: Option<f32>,
    /// Storage bytes a second; `None` until a second reading, or when
    /// this window may not see it (another user's, without Show
    /// everything).
    pub disk: Option<f64>,
    /// Percent of its busiest GPU engine; `None` for a process that has
    /// no GPU open (or one this user may not read).
    pub gpu: Option<f32>,
    pub private_bytes: u64,
    pub threads: u32,
    pub state: char,
    pub kernel: bool,
    pub ppid: i32,
    pub nice: i32,
    /// CPU time over its whole life, in clock ticks.
    pub ticks: u64,
    pub resident_bytes: u64,
}

/// A window's process and the processes it started.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub lead: Entry,
    /// The window's class (`firefox`) and title, from Hyprland — the
    /// first window, when a process has several.
    pub class: String,
    pub title: String,
    pub windows: usize,
    /// The lead first, then the rest in tree order.
    pub members: Vec<Entry>,
}

impl App {
    /// Known only when every member has a reading.
    pub fn cpu(&self) -> Option<f32> {
        self.members.iter().map(|m| m.cpu).sum()
    }

    /// The members that use the GPU, added up; `None` when none does.
    pub fn gpu(&self) -> Option<f32> {
        let used: Vec<f32> = self.members.iter().filter_map(|m| m.gpu).collect();
        (!used.is_empty()).then(|| used.iter().sum::<f32>().min(100.0))
    }

    /// Known only when every member's is.
    pub fn disk(&self) -> Option<f64> {
        self.members.iter().map(|m| m.disk).sum()
    }

    pub fn private_bytes(&self) -> u64 {
        self.members.iter().map(|m| m.private_bytes).sum()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub apps: Vec<App>,
    pub background: Vec<Entry>,
}

/// Builds the table from one refresh. `user_name` turns a uid into a
/// name; `me` is the uid running this.
pub fn table(
    snapshot: &Snapshot,
    usage: &Use,
    windows: &[Window],
    me: u32,
    user_name: &mut dyn FnMut(u32) -> String,
) -> Table {
    let by_pid: HashMap<i32, &Proc> = snapshot.procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in &snapshot.procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut entry = |p: &Proc| {
        let id = Id { pid: p.pid, start: p.start };
        Entry {
            id,
            name: p.name.clone(),
            command: p.command.clone(),
            user: user_name(p.uid),
            own: p.uid == me,
            cpu: usage.cpu.get(&id).copied(),
            disk: usage.disk.get(&id).copied(),
            gpu: usage.gpu.get(&id).copied(),
            private_bytes: p.private_bytes,
            threads: p.threads,
            state: p.state,
            kernel: p.kernel,
            ppid: p.ppid,
            nice: p.nice,
            ticks: p.ticks,
            resident_bytes: p.resident_bytes,
        }
    };

    // The first window per process, in Hyprland's order, and how many.
    let mut window_of: HashMap<i32, (&Window, usize)> = HashMap::new();
    for w in windows {
        window_of.entry(w.pid).and_modify(|(_, n)| *n += 1).or_insert((w, 1));
    }
    let leads: HashSet<i32> = window_of.keys().copied().filter(|pid| by_pid.contains_key(pid)).collect();

    let mut claimed: HashSet<i32> = HashSet::new();
    let mut apps = Vec::new();
    let mut lead_pids: Vec<i32> = leads.iter().copied().collect();
    lead_pids.sort_unstable();
    for pid in lead_pids {
        let (window, count) = window_of[&pid];
        let mut members = Vec::new();
        // Depth first, stopping at another window's process: that one is
        // an app of its own.
        let mut stack = vec![pid];
        while let Some(next) = stack.pop() {
            if next != pid && leads.contains(&next) {
                continue;
            }
            let Some(p) = by_pid.get(&next) else { continue };
            if !claimed.insert(next) {
                continue;
            }
            members.push(entry(p));
            if let Some(kids) = children.get(&next) {
                let mut kids = kids.clone();
                kids.sort_unstable_by(|a, b| b.cmp(a));
                stack.extend(kids);
            }
        }
        let lead = members[0].clone();
        apps.push(App { lead, class: window.class.clone(), title: window.title.clone(), windows: count, members });
    }
    let background = snapshot.procs.iter().filter(|p| !claimed.contains(&p.pid)).map(&mut entry).collect();
    Table { apps, background }
}

/// `pid` and every process above it, up to the first that has no
/// parent in the snapshot. For this window that is the session it runs
/// in — the compositor, the session manager, the user's systemd —
/// which is what ending or freezing must not take away from under it.
pub fn ancestry(snapshot: &Snapshot, pid: i32) -> HashSet<i32> {
    let parent: HashMap<i32, i32> = snapshot.procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut out = HashSet::new();
    let mut at = pid;
    while at > 0 && out.insert(at) {
        match parent.get(&at) {
            Some(&up) => at = up,
            None => break,
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Cpu,
    Memory,
    Disk,
    Gpu,
    Pid,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

impl Default for Sort {
    /// What a person opens a process manager to find out.
    fn default() -> Sort {
        Sort { column: Column::Cpu, descending: true }
    }
}

impl Sort {
    /// A click on a column's header: the same column flips, a new one
    /// starts in the order people want it — biggest first for numbers, A
    /// to Z for words.
    pub fn clicked(self, column: Column) -> Sort {
        if self.column == column {
            Sort { descending: !self.descending, ..self }
        } else {
            Sort { column, descending: matches!(column, Column::Cpu | Column::Memory | Column::Disk | Column::Gpu) }
        }
    }
}

/// What the list shows besides its data.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub sort: Sort,
    pub query: String,
    /// Apps opened to show their processes.
    pub expanded: HashSet<Id>,
    pub show_kernel: bool,
    /// What a `port:`/`file:` search found; replaces the text search
    /// while it is set.
    pub only: Option<HashSet<i32>>,
}

/// One line of the list, top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Heading { label: &'static str, count: usize },
    App { app: App, open: bool },
    /// A process inside an open app.
    Member(Entry),
    Process(Entry),
}

/// What a row is sorted by.
struct Key<'a> {
    name: &'a str,
    cpu: Option<f32>,
    memory: u64,
    disk: Option<f64>,
    gpu: Option<f32>,
    id: Id,
    user: &'a str,
}

fn compare(sort: Sort, a: Key, b: Key) -> Ordering {
    let order = match sort.column {
        Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        // No reading yet sorts as the least busy.
        Column::Cpu => a.cpu.unwrap_or(-1.0).total_cmp(&b.cpu.unwrap_or(-1.0)),
        Column::Memory => a.memory.cmp(&b.memory),
        Column::Disk => a.disk.unwrap_or(-1.0).total_cmp(&b.disk.unwrap_or(-1.0)),
        Column::Gpu => a.gpu.unwrap_or(-1.0).total_cmp(&b.gpu.unwrap_or(-1.0)),
        Column::Pid => a.id.pid.cmp(&b.id.pid),
        Column::User => a.user.cmp(b.user),
    };
    // Ties fall back to the pid, so equal rows keep still between frames.
    let order = order.then(a.id.pid.cmp(&b.id.pid));
    if sort.descending {
        order.reverse()
    } else {
        order
    }
}

fn key(e: &Entry) -> Key<'_> {
    Key { name: &e.name, cpu: e.cpu, memory: e.private_bytes, disk: e.disk, gpu: e.gpu, id: e.id, user: &e.user }
}

fn app_key(a: &App) -> Key<'_> {
    Key { name: &a.lead.name, cpu: a.cpu(), memory: a.private_bytes(), disk: a.disk(), gpu: a.gpu(), id: a.lead.id, user: &a.lead.user }
}

fn matches(e: &Entry, query: &str) -> bool {
    query.is_empty()
        || query.starts_with("port:")
        || query.starts_with("file:")
        || e.name.to_lowercase().contains(query)
        || e.command.to_lowercase().contains(query)
        || e.id.pid.to_string() == query
}

/// The lines to draw. A search opens every app it finds something inside,
/// showing only what matched — unless the app itself matched (its name,
/// class or window title), which shows the app as it is.
pub fn lines(table: &Table, view: &View) -> Vec<Line> {
    let query = view.query.trim().to_lowercase();
    let hit = |e: &Entry| match &view.only {
        Some(set) => set.contains(&e.id.pid),
        None => matches(e, &query),
    };
    let mut apps: Vec<(App, bool)> = Vec::new();
    for app in &table.apps {
        let itself = view.only.is_none() && query.is_empty()
            || view.only.is_none() && (app.title.to_lowercase().contains(&query) || app.class.to_lowercase().contains(&query))
            || hit(&app.lead);
        if itself {
            apps.push((app.clone(), view.expanded.contains(&app.lead.id)));
            continue;
        }
        let found: Vec<Entry> = app.members.iter().filter(|m| hit(m)).cloned().collect();
        if !found.is_empty() {
            apps.push((App { members: found, ..app.clone() }, true));
        }
    }
    apps.sort_by(|(a, _), (b, _)| compare(view.sort, app_key(a), app_key(b)));
    let mut background: Vec<&Entry> =
        table.background.iter().filter(|e| (view.show_kernel || !e.kernel) && hit(e)).collect();
    background.sort_by(|a, b| compare(view.sort, key(a), key(b)));

    let mut out = Vec::new();
    if !apps.is_empty() {
        out.push(Line::Heading { label: "Apps", count: apps.len() });
        for (app, open) in apps {
            let mut members: Vec<Entry> = app.members.clone();
            members.sort_by(|a, b| compare(view.sort, key(a), key(b)));
            out.push(Line::App { app, open });
            if open {
                out.extend(members.into_iter().map(Line::Member));
            }
        }
    }
    if !background.is_empty() {
        out.push(Line::Heading { label: "Background processes", count: background.len() });
        out.extend(background.into_iter().cloned().map(Line::Process));
    }
    out
}

/// Every process in the table, apps' and background alike.
pub fn entries(table: &Table) -> impl Iterator<Item = &Entry> {
    table.apps.iter().flat_map(|a| a.members.iter()).chain(table.background.iter())
}

/// The Details tab's lines: every process, flat or as a tree, each with
/// its depth. In a tree a search keeps the way down to what it found, so
/// a match is never shown without its parents. `only`, when given, is
/// the set a `port:`/`file:` lookup found.
pub fn details(table: &Table, view: &View, tree: bool) -> Vec<(usize, Entry)> {
    let query = view.query.trim().to_lowercase();
    let only = view.only.as_ref();
    let lookup = only.is_some();
    let all: Vec<&Entry> = entries(table).filter(|e| view.show_kernel || !e.kernel).collect();
    let hit = |e: &Entry| match only {
        Some(set) => set.contains(&e.id.pid),
        None => matches(e, &query),
    };
    if !tree {
        let mut flat: Vec<&Entry> = all.into_iter().filter(|e| hit(e)).collect();
        flat.sort_by(|a, b| compare(view.sort, key(a), key(b)));
        return flat.into_iter().map(|e| (0, e.clone())).collect();
    }
    let present: HashSet<i32> = all.iter().map(|e| e.id.pid).collect();
    let mut children: HashMap<i32, Vec<&Entry>> = HashMap::new();
    let mut roots = Vec::new();
    for e in &all {
        if present.contains(&e.ppid) && e.ppid != e.id.pid {
            children.entry(e.ppid).or_default().push(e);
        } else {
            roots.push(*e);
        }
    }
    // What to keep: everything when nothing is searched, otherwise each
    // match and every ancestor of one.
    let keep: Option<HashSet<i32>> = (lookup || !query.is_empty()).then(|| {
        let parent: HashMap<i32, i32> = all.iter().map(|e| (e.id.pid, e.ppid)).collect();
        let mut keep = HashSet::new();
        for e in all.iter().filter(|e| hit(e)) {
            let mut at = e.id.pid;
            while keep.insert(at) {
                match parent.get(&at) {
                    Some(&up) if present.contains(&up) => at = up,
                    _ => break,
                }
            }
        }
        keep
    });
    let sort = |list: &mut Vec<&Entry>| list.sort_by(|a, b| compare(view.sort, key(a), key(b)));
    sort(&mut roots);
    let mut out = Vec::new();
    let mut stack: Vec<(usize, &Entry)> = roots.into_iter().rev().map(|e| (0, e)).collect();
    while let Some((depth, e)) = stack.pop() {
        if keep.as_ref().is_some_and(|k| !k.contains(&e.id.pid)) {
            continue;
        }
        out.push((depth, e.clone()));
        if let Some(kids) = children.get(&e.id.pid) {
            let mut kids = kids.clone();
            sort(&mut kids);
            stack.extend(kids.into_iter().rev().map(|k| (depth + 1, k)));
        }
    }
    out
}

/// `id` and everything under it, deepest first — the order End process
/// tree signals in, so a parent never sees its children die and starts
/// new ones before it is asked to stop itself.
pub fn tree_of(table: &Table, id: Id) -> Vec<Id> {
    let all: Vec<&Entry> = entries(table).collect();
    let mut children: HashMap<i32, Vec<Id>> = HashMap::new();
    for e in &all {
        children.entry(e.ppid).or_default().push(e.id);
    }
    let mut order = Vec::new();
    let mut stack = vec![(id, false)];
    let mut seen = HashSet::new();
    while let Some((at, expanded)) = stack.pop() {
        if expanded {
            order.push(at);
            continue;
        }
        if !seen.insert(at.pid) {
            continue;
        }
        stack.push((at, true));
        for kid in children.get(&at.pid).into_iter().flatten() {
            stack.push((*kid, false));
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::{CpuTimes, Proc};

    fn proc(pid: i32, ppid: i32, name: &str, uid: u32) -> Proc {
        Proc {
            pid,
            start: pid as u64 * 10,
            name: name.into(),
            command: format!("/usr/bin/{name}"),
            state: 'S',
            ppid,
            ticks: 0,
            threads: 1,
            private_bytes: pid as u64 * 1000,
            uid,
            kernel: false,
            io: None,
            nice: 0,
            resident_bytes: 0,
            user_ticks: 0,
            system_ticks: 0,
            minflt: 0,
            majflt: 0,
            vsize: 0,
            processor: 0,
            policy: 0,
            verbose: None,
        }
    }

    fn window(pid: i32, class: &str, title: &str) -> Window {
        Window { pid, class: class.into(), title: title.into() }
    }

    /// systemd, a browser with two content processes, a terminal with a
    /// shell that launched an editor with a window of its own, and a
    /// daemon.
    fn table_of(shares: &HashMap<Id, f32>) -> Table {
        let usage = Use { cpu: shares.clone(), ..Use::default() };
        let snapshot = Snapshot {
            procs: vec![
                proc(1, 0, "systemd", 0),
                proc(100, 1, "firefox", 1000),
                proc(101, 100, "Isolated Web Co", 1000),
                proc(102, 100, "Isolated Web Co", 1000),
                proc(200, 1, "kitty", 1000),
                proc(201, 200, "zsh", 1000),
                proc(202, 201, "gedit", 1000),
                proc(300, 1, "pipewire", 1000),
            ],
            cpu: CpuTimes::default(),
            ..Snapshot::default()
        };
        let windows = [window(100, "firefox", "The Daily Prophet"), window(200, "kitty", "~"), window(202, "gedit", "spells.txt"), window(100, "firefox", "Owl Post")];
        table(&snapshot, &usage, &windows, 1000, &mut |uid| if uid == 0 { "root".into() } else { "voldemort".into() })
    }

    fn pids(app: &App) -> Vec<i32> {
        app.members.iter().map(|m| m.id.pid).collect()
    }

    #[test]
    fn an_app_is_its_window_process_and_what_it_started() {
        let t = table_of(&HashMap::new());
        let firefox = t.apps.iter().find(|a| a.class == "firefox").unwrap();
        assert_eq!(pids(firefox), [100, 101, 102]);
        assert_eq!(firefox.windows, 2);
        assert_eq!(firefox.title, "The Daily Prophet", "the first window's title");
        assert_eq!(firefox.private_bytes(), (100 + 101 + 102) * 1000);
    }

    /// The editor launched from the terminal has a window of its own: it
    /// is its own app, and the terminal does not count it.
    #[test]
    fn a_window_inside_another_apps_tree_is_an_app_of_its_own() {
        let t = table_of(&HashMap::new());
        let kitty = t.apps.iter().find(|a| a.class == "kitty").unwrap();
        assert_eq!(pids(kitty), [200, 201]);
        assert!(t.apps.iter().any(|a| a.class == "gedit"));
        let bg: Vec<i32> = t.background.iter().map(|e| e.id.pid).collect();
        assert_eq!(bg, [1, 300]);
    }

    #[test]
    fn an_apps_cpu_is_known_only_when_every_process_in_it_has_a_reading() {
        let mut shares = HashMap::new();
        shares.insert(Id { pid: 100, start: 1000 }, 10.0);
        shares.insert(Id { pid: 101, start: 1010 }, 5.0);
        let t = table_of(&shares);
        assert_eq!(t.apps.iter().find(|a| a.class == "firefox").unwrap().cpu(), None);
        shares.insert(Id { pid: 102, start: 1020 }, 2.5);
        let t = table_of(&shares);
        assert_eq!(t.apps.iter().find(|a| a.class == "firefox").unwrap().cpu(), Some(17.5));
    }

    #[test]
    fn ownership_and_user_names_come_from_the_uid() {
        let t = table_of(&HashMap::new());
        let systemd = t.background.iter().find(|e| e.id.pid == 1).unwrap();
        assert_eq!((systemd.user.as_str(), systemd.own), ("root", false));
        assert!(t.background.iter().find(|e| e.id.pid == 300).unwrap().own);
    }

    #[test]
    fn the_list_is_apps_then_background_each_under_a_heading() {
        let t = table_of(&HashMap::new());
        let view = View { sort: Sort { column: Column::Name, descending: false }, ..View::default() };
        let l = lines(&t, &view);
        assert_eq!(l[0], Line::Heading { label: "Apps", count: 3 });
        let names: Vec<&str> = l
            .iter()
            .filter_map(|line| match line {
                Line::App { app, .. } => Some(app.lead.name.as_str()),
                Line::Process(e) => Some(e.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["firefox", "gedit", "kitty", "pipewire", "systemd"]);
    }

    #[test]
    fn an_open_app_lists_its_processes_under_it() {
        let t = table_of(&HashMap::new());
        let mut view = View::default();
        view.expanded.insert(Id { pid: 100, start: 1000 });
        let l = lines(&t, &view);
        let at = l.iter().position(|line| matches!(line, Line::App { app, open: true } if app.class == "firefox")).unwrap();
        assert!(matches!(&l[at + 1], Line::Member(_)));
        assert_eq!(l.iter().filter(|line| matches!(line, Line::Member(_))).count(), 3);
    }

    /// Searching for a content process opens its app to show just it;
    /// searching for the app by its window title shows the app closed.
    #[test]
    fn a_search_opens_an_app_to_what_it_found_inside() {
        let t = table_of(&HashMap::new());
        let l = lines(&t, &View { query: "isolated".into(), ..View::default() });
        assert!(matches!(&l[1], Line::App { app, open: true } if app.members.len() == 2));
        let l = lines(&t, &View { query: "daily prophet".into(), ..View::default() });
        assert_eq!(l.len(), 2, "the heading and firefox, closed");
        assert!(matches!(&l[1], Line::App { open: false, .. }));
    }

    #[test]
    fn a_header_click_flips_the_same_column_and_starts_a_new_one_sensibly() {
        let s = Sort::default();
        assert_eq!(s.clicked(Column::Cpu), Sort { column: Column::Cpu, descending: false });
        assert_eq!(s.clicked(Column::Name), Sort { column: Column::Name, descending: false });
        assert_eq!(s.clicked(Column::Memory), Sort { column: Column::Memory, descending: true });
    }

    #[test]
    fn ancestry_climbs_to_the_top_and_stops_at_a_loop() {
        let snapshot = Snapshot {
            procs: vec![proc(1, 0, "systemd", 0), proc(50, 1, "Hyprland", 1000), proc(60, 50, "procman", 1000), proc(70, 1, "other", 1000)],
            ..Snapshot::default()
        };
        assert_eq!(ancestry(&snapshot, 60), HashSet::from([60, 50, 1]));
        let looped = Snapshot { procs: vec![proc(5, 6, "a", 0), proc(6, 5, "b", 0)], ..Snapshot::default() };
        assert_eq!(ancestry(&looped, 5), HashSet::from([5, 6]));
    }

    #[test]
    fn the_tree_puts_children_under_parents_and_a_search_keeps_the_way_down() {
        let t = table_of(&HashMap::new());
        let view = View { sort: Sort { column: Column::Pid, descending: false }, ..View::default() };
        let tree: Vec<(usize, i32)> = details(&t, &view, true).into_iter().map(|(d, e)| (d, e.id.pid)).collect();
        assert_eq!(tree, [(0, 1), (1, 100), (2, 101), (2, 102), (1, 200), (2, 201), (3, 202), (1, 300)]);
        let found: Vec<(usize, i32)> =
            details(&t, &View { query: "gedit".into(), ..view.clone() }, true).into_iter().map(|(d, e)| (d, e.id.pid)).collect();
        assert_eq!(found, [(0, 1), (1, 200), (2, 201), (3, 202)]);
        let flat = details(&t, &View { query: "gedit".into(), ..view.clone() }, false);
        assert_eq!(flat.len(), 1);
        let looked = View { query: "port:443".into(), only: Some(HashSet::from([101])), ..view.clone() };
        assert_eq!(details(&t, &looked, false).len(), 1, "a lookup's set replaces the text search");
        let l = lines(&t, &looked);
        assert!(matches!(&l[1], Line::App { app, open: true } if app.members.len() == 1 && app.members[0].id.pid == 101), "and Processes answers it too");
    }

    #[test]
    fn a_process_tree_ends_deepest_first() {
        let t = table_of(&HashMap::new());
        let order: Vec<i32> = tree_of(&t, Id { pid: 200, start: 2000 }).into_iter().map(|i| i.pid).collect();
        assert_eq!(order.last(), Some(&200), "the parent last");
        assert!(order.iter().position(|p| *p == 202) < order.iter().position(|p| *p == 201), "grandchild before child");
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn kernel_threads_are_hidden_until_asked_for() {
        let mut t = table_of(&HashMap::new());
        t.background[0].kernel = true;
        assert!(!lines(&t, &View::default()).iter().any(|l| matches!(l, Line::Process(e) if e.id.pid == 1)));
        let all = View { show_kernel: true, ..View::default() };
        assert!(lines(&t, &all).iter().any(|l| matches!(l, Line::Process(e) if e.id.pid == 1)));
    }
}
