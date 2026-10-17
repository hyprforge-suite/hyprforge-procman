//! Rules: something to do to a program every time it runs — the job
//! Process Lasso and ananicy do, for this suite.
//!
//! A rule names a program and says what to do once it starts: run it at
//! a lower priority, cap its memory or CPU, or warn when it passes a
//! size. They are kept in `procman-rules.toml` in the suite's config
//! directory, written by the Rules tab, and applied by a user service,
//! `hyprforge-procman-rules`.
//!
//! Three limits on what a rule may do, each a fact about this machine:
//!
//! - **Priority only goes down.** An unprivileged process may raise a
//!   nice value and never lower it (`RLIMIT_NICE` is 0 here), so a rule
//!   asking for 0–19 works and one asking for less never could — it is
//!   refused when the file is read, not left to fail at every start.
//! - **A memory or CPU cap is the limits code's**, which moves a process
//!   out of a unit it shares before limiting it: `cargo` in a terminal
//!   gets a scope of its own, and the terminal is left alone.
//! - **Noticing a start is a scan of `/proc`.** Hearing starts from the
//!   kernel needs root, and systemd's "new unit" signal never sees a
//!   program started in a shell. So every couple of seconds the service
//!   lists pids and reads only the new ones — and with no rules it does
//!   not scan at all.
//!
//! A file that exists and doesn't parse is an error the Rules tab shows,
//! never "no rules" (CLAUDE.md): the service then applies nothing new
//! and says why, rather than silently running with none.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where the rules are kept.
pub fn path() -> PathBuf {
    hyprforge_paths::hyprforge_config_dir().join("procman-rules.toml")
}

/// One rule, as the file holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rule {
    /// The program, as the Name column shows it: `cargo`, `blender`.
    pub program: String,
    /// A nice value to run it at, 0 (as usual) to 19 (last in line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// A memory cap, as people write it: `8 GB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// A CPU cap, as a percent of the whole machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    /// Warn when it holds more than this: `4 GB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_memory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct File {
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// A rule read and checked: what it does, in the units it is done in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub program: String,
    pub priority: Option<i32>,
    pub limits: crate::limits::Limits,
    /// Bytes.
    pub warn_memory: Option<u64>,
}

/// What is wrong with one rule, in words, or the rule as it will be done.
pub fn check(rule: &Rule) -> Result<Checked, String> {
    let program = rule.program.trim();
    if program.is_empty() {
        return Err("a rule needs a program to apply to".into());
    }
    if let Some(p) = rule.priority {
        if !(0..=19).contains(&p) {
            return Err(format!("{program}: priority {p} — only 0 to 19 can be set without administrator access"));
        }
    }
    let size = |what: &str, text: &Option<String>| -> Result<Option<u64>, String> {
        match text.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(t) => crate::limits::parse_size(t).map(Some).ok_or_else(|| format!("{program}: {what} \"{t}\" — write a size like 4 GB, 64 MB at the least")),
        }
    };
    let memory = size("memory", &rule.memory)?;
    let warn_memory = size("warn_memory", &rule.warn_memory)?;
    if let Some(c) = rule.cpu {
        if !(1..=100).contains(&c) {
            return Err(format!("{program}: cpu {c} — a percent of the whole machine, 1 to 100"));
        }
    }
    if rule.priority.is_none() && memory.is_none() && rule.cpu.is_none() && warn_memory.is_none() {
        return Err(format!("{program}: the rule does nothing — give it a priority, a cap or a warning"));
    }
    Ok(Checked { program: program.to_string(), priority: rule.priority, limits: crate::limits::Limits { memory, cpu_percent: rule.cpu }, warn_memory })
}

/// The rules in force, or why the file can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// No file yet: first run, nothing to do.
    None,
    Rules(Vec<Checked>),
    /// The file is there and isn't usable. Never collapsed into `None`.
    Broken(String),
}

pub fn parse(text: &str) -> Result<File, String> {
    toml::from_str(text).map_err(|e| format!("procman-rules.toml doesn't read: {e}"))
}

pub fn load(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::None,
        Err(e) => Loaded::Broken(format!("procman-rules.toml couldn't be read: {e}")),
        Ok(text) => match parse(&text) {
            Err(e) => Loaded::Broken(e),
            Ok(file) => match file.rules.iter().map(check).collect::<Result<Vec<_>, _>>() {
                Ok(rules) => Loaded::Rules(rules),
                Err(e) => Loaded::Broken(e),
            },
        },
    }
}

/// Writes `file` whole, atomically — and only if every rule checks, so
/// the editor can never save a file the service would refuse.
pub fn save(path: &Path, file: &File) -> Result<(), String> {
    for r in &file.rules {
        check(r)?;
    }
    let text = toml::to_string(file).map_err(|e| e.to_string())?;
    hyprforge_paths::write_atomic(path, &format!("# Written by Processes → Rules. Applied by hyprforge-procman-rules.\n{text}")).map_err(|e| e.to_string())
}

/// One process, as the scan sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub pid: i32,
    pub start: u64,
    pub name: String,
    pub nice: i32,
    pub own: bool,
}

/// The rule for a process: the first naming it.
pub fn rule_for<'a>(rules: &'a [Checked], p: &Seen) -> Option<&'a Checked> {
    rules.iter().find(|r| r.program == p.name)
}

/// What to do to a process that just appeared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Set its nice value — only ever a raise.
    Nice { pid: i32, to: i32 },
    /// Cap it, through the limits code.
    Limit { pid: i32, name: String, limits: crate::limits::Limits },
}

/// For each process not seen before, what its rule says to do. Only
/// this user's own processes; a nice value already at or past the rule's
/// is left alone (setting it would be a lowering, which can't be done).
pub fn on_start(rules: &[Checked], fresh: &[Seen]) -> Vec<Act> {
    let mut out = Vec::new();
    for p in fresh.iter().filter(|p| p.own) {
        let Some(r) = rule_for(rules, p) else { continue };
        if let Some(to) = r.priority {
            if to > p.nice {
                out.push(Act::Nice { pid: p.pid, to });
            }
        }
        if !r.limits.is_none() {
            out.push(Act::Limit { pid: p.pid, name: p.name.clone(), limits: r.limits });
        }
    }
    out
}

/// Whether a process sits under another that its same rule names — then
/// that one's cap moves it along, and a cap of its own would split the
/// app: Claude's desktop app is a dozen `claude-desktop` processes, and
/// without this each would have been given a scope of its own.
pub fn under_another(pid: i32, name: &str, parents: &HashMap<i32, i32>, names: &HashMap<i32, String>) -> bool {
    let mut at = pid;
    let mut steps = 0;
    while let Some(&up) = parents.get(&at) {
        if up <= 1 || steps > 64 {
            return false;
        }
        if names.get(&up).is_some_and(|n| n == name) {
            return true;
        }
        at = up;
        steps += 1;
    }
    false
}

/// The app a cap would take with it that isn't this one: a process in
/// the tree, other than its root, that owns a window. A rule naming
/// Hyprland would otherwise have moved Ghostty, waybar and every app
/// Hyprland started into a scope capped for the compositor; one naming a
/// shell would take the browser it launched.
pub fn takes_another_app(tree: &[i32], window_pids: &HashSet<i32>) -> Option<i32> {
    tree.iter().skip(1).copied().find(|p| window_pids.contains(p))
}

/// Processes past their rule's warning size that haven't been warned
/// about yet: `(pid, name, bytes held, the size warned at)`.
pub fn over(rules: &[Checked], held: &[(Seen, u64)], warned: &HashSet<(i32, u64)>) -> Vec<(i32, String, u64, u64)> {
    held.iter()
        .filter(|(p, _)| p.own && !warned.contains(&(p.pid, p.start)))
        .filter_map(|(p, bytes)| {
            let at = rule_for(rules, p)?.warn_memory?;
            (*bytes > at).then(|| (p.pid, p.name.clone(), *bytes, at))
        })
        .collect()
}

/// The pids in a `/proc` and when each started, without reading anything
/// else — what tells a new process from one already seen, cheaply.
pub fn pids(root: &Path) -> HashMap<i32, u64> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| Some((pid, crate::proc::start_time_in(root, pid)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(program: &str) -> Rule {
        Rule { program: program.into(), ..Rule::default() }
    }

    #[test]
    fn a_rule_reads_in_its_own_units_and_a_wrong_one_says_what_is_wrong() {
        let ok = check(&Rule { priority: Some(10), memory: Some("8 GB".into()), ..rule("cargo") }).unwrap();
        assert_eq!(ok.limits.memory, Some(8 << 30));
        assert!(check(&Rule { priority: Some(-5), ..rule("game") }).unwrap_err().contains("administrator"), "can't be done unprivileged, so refused up front");
        assert!(check(&Rule { cpu: Some(150), ..rule("x") }).unwrap_err().contains("1 to 100"));
        assert!(check(&rule("idle")).unwrap_err().contains("does nothing"));
        assert!(check(&Rule { priority: Some(5), ..rule(" ") }).is_err());
    }

    /// The CLAUDE.md rule: a missing file is first run, a broken one is
    /// a problem to show — never the same thing.
    #[test]
    fn a_missing_file_is_no_rules_and_a_broken_one_is_never_mistaken_for_it() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(load(&t.path().join("procman-rules.toml")), Loaded::None);
        std::fs::write(t.path().join("procman-rules.toml"), "[[rule]\nprogram = ").unwrap();
        assert!(matches!(load(&t.path().join("procman-rules.toml")), Loaded::Broken(_)));
        std::fs::write(t.path().join("procman-rules.toml"), "[[rule]]\nprogram = \"x\"\npriority = -3\n").unwrap();
        assert!(matches!(load(&t.path().join("procman-rules.toml")), Loaded::Broken(e) if e.contains("administrator")));
    }

    #[test]
    fn what_is_saved_reads_back_and_a_bad_rule_is_never_saved() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-rules.toml");
        let file = File { rules: vec![Rule { priority: Some(10), warn_memory: Some("4 GB".into()), ..rule("cargo") }] };
        save(&p, &file).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(&p).unwrap()).unwrap(), file);
        let bad = File { rules: vec![Rule { priority: Some(-1), ..rule("x") }] };
        assert!(save(&p, &bad).is_err());
        assert_eq!(parse(&std::fs::read_to_string(&p).unwrap()).unwrap(), file, "the good file is untouched");
    }

    fn seen(pid: i32, name: &str, nice: i32) -> Seen {
        Seen { pid, start: 1, name: name.into(), nice, own: true }
    }

    #[test]
    fn a_new_process_gets_its_rule_and_a_priority_is_only_ever_raised() {
        let rules = vec![check(&Rule { priority: Some(10), memory: Some("8 GB".into()), ..rule("cargo") }).unwrap()];
        let acts = on_start(&rules, &[seen(5, "cargo", 0), seen(6, "rustc", 0), seen(7, "cargo", 15)]);
        assert_eq!(acts[0], Act::Nice { pid: 5, to: 10 });
        assert!(matches!(acts[1], Act::Limit { pid: 5, .. }));
        assert!(!acts.iter().any(|a| matches!(a, Act::Nice { pid: 7, .. })), "already lower than the rule: lowering it back can't be done");
        assert!(!acts.iter().any(|a| matches!(a, Act::Nice { pid: 6, .. } | Act::Limit { pid: 6, .. })), "no rule, nothing done");
        let other = Seen { own: false, ..seen(8, "cargo", 0) };
        assert!(on_start(&rules, &[other]).is_empty(), "another user's process is never touched");
    }

    /// Claude's tree here: the app, its zygote, and helpers under both,
    /// all named alike. Only the top one is capped.
    #[test]
    fn a_process_under_another_its_rule_names_is_left_to_that_ones_cap() {
        let parents = HashMap::from([(232772, 232763), (232774, 232772), (233517, 232774), (232763, 1192)]);
        let names: HashMap<i32, String> = [232763, 232772, 232774, 233517].into_iter().map(|p| (p, "claude-desktop".to_string())).chain([(1192, "systemd".to_string())]).collect();
        assert!(!under_another(232763, "claude-desktop", &parents, &names));
        assert!(under_another(233517, "claude-desktop", &parents, &names));
        assert!(!under_another(233517, "cargo", &parents, &names), "a different rule's name doesn't count");
    }

    /// The dry run that found it: a rule for Hyprland, whose tree holds
    /// Ghostty's window.
    #[test]
    fn a_cap_that_would_take_another_app_along_is_found() {
        let windows: HashSet<i32> = [13060, 232763].into_iter().collect();
        assert_eq!(takes_another_app(&[1287, 1354, 13060, 1350], &windows), Some(13060));
        assert_eq!(takes_another_app(&[232763, 232772], &windows), None, "its own window is no other app");
        assert_eq!(takes_another_app(&[5000, 5001], &windows), None);
    }

    #[test]
    fn a_warning_is_given_once_per_process_past_its_size() {
        let rules = vec![check(&Rule { warn_memory: Some("1 GB".into()), ..rule("blender") }).unwrap()];
        let held = [(seen(5, "blender", 0), 2 << 30), (seen(6, "blender", 0), 100 << 20)];
        let first = over(&rules, &held, &HashSet::new());
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, 5);
        assert!(over(&rules, &held, &[(5, 1)].into_iter().collect()).is_empty(), "warned once");
    }

    #[test]
    fn this_machines_proc_lists_this_process_with_its_start() {
        let me = std::process::id() as i32;
        let all = pids(Path::new("/proc"));
        assert_eq!(all.get(&me).copied(), crate::proc::start_time(me));
    }
}
