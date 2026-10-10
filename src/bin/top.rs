//! hyprforge-top — atop for this suite, in a terminal.
//!
//! Live by default; `-r` replays the recorder's journal. Run as yourself
//! it shows what you may read and says so; under `sudo` it shows
//! everything, and a journal you may not open is asked of the helper,
//! which checks polkit first.

use hyprforge_procman::journal::Reader;
use hyprforge_procman::recorder;
use hyprforge_procman::report::{self, Kind};
use hyprforge_procman::sample::Id;
use hyprforge_procman::signal::{self, How};
use hyprforge_procman::tui::render;
use hyprforge_procman::tui::source::{self, Extras, Live, Replay, Screen};
use hyprforge_procman::tui::state::{self, Action, Key, State, View};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

const USAGE: &str = "\
hyprforge-top — what the machine is doing, atop's way

USAGE
    hyprforge-top [-i SECONDS]          live, refreshing every SECONDS (default 2)
    hyprforge-top -r [DAY|FILE] [-b HH:MM[:SS]]
                                        replay the recorder's journal: a day
                                        (2026-10-10, default today) or a file,
                                        starting at a time of day
    hyprforge-top report KIND [DAY|FILE] [--from HH:MM[:SS]] [--to HH:MM[:SS]]
                         [--step 10m] [--top N] [--json]
                                        what a day added up to, atopsar's
                                        way. KIND is cpu, mem, disk, net,
                                        psi or top (the programs that used
                                        the most). Steps are 30s, 10m, 1h…

Inside, press h for every key. The journal is root's: run with sudo, or
the helper asks for your password through polkit.
";

enum Mode {
    Live(Box<Live>, Duration),
    Replay(Replay),
}

/// The journal `-r` or `report` names: a day or a file, today if neither.
fn journal(what: Option<String>) -> (String, std::io::Result<Reader>) {
    let what = what.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let directory = recorder::load_config(std::path::Path::new(recorder::CONFIG_PATH)).map(|c| c.directory).unwrap_or_else(|_| recorder::Config::default().directory);
    let reader = source::open_journal(&what, &directory);
    (what, reader)
}

fn fail(message: impl std::fmt::Display) -> std::process::ExitCode {
    eprintln!("hyprforge-top: {message}");
    std::process::ExitCode::FAILURE
}

/// `30s`, `10m`, `1h`, or plain seconds.
fn parse_step(text: &str) -> Option<i64> {
    let (number, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => text.split_at(i),
        None => (text, "s"),
    };
    let n: i64 = number.parse().ok().filter(|&n| n > 0)?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    };
    Some(n * scale * 1000)
}

fn report_main(args: &[String]) -> std::process::ExitCode {
    let Some(kind) = args.first().and_then(|k| Kind::parse(k)) else {
        return fail("report takes cpu, mem, disk, net, psi or top — see --help");
    };
    let flags = ["--from", "--to", "--step", "--top"];
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    // The day or file: the one argument that is neither a flag nor a flag's value.
    let what = args.iter().enumerate().skip(1).find(|(i, a)| !a.starts_with("--") && !flags.contains(&args[i - 1].as_str())).map(|(_, a)| a.clone());
    let (what, reader) = journal(what);
    let mut reader = match reader {
        Ok(r) => r,
        Err(e) => return fail(format!("couldn't open the journal for {what}: {e}")),
    };
    let time = |name: &str, default: i64| -> Result<i64, std::process::ExitCode> {
        match value(name) {
            None => Ok(default),
            Some(t) => state::parse_time(&t).and_then(|at| report::at_time(&reader, at)).ok_or_else(|| fail(format!("{name} {t:?} isn't a time — HH:MM or HH:MM:SS"))),
        }
    };
    let from_ms = match time("--from", i64::MIN / 2) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let to_ms = match time("--to", i64::MAX / 2) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let step_ms = match value("--step").map(|s| parse_step(&s)) {
        None => 600_000,
        Some(Some(ms)) => ms,
        Some(None) => return fail("--step takes 30s, 10m, 1h or a number of seconds"),
    };
    let top = match value("--top").map(|n| n.parse::<usize>()) {
        None => 15,
        Some(Ok(n)) if n > 0 => n,
        Some(_) => return fail("--top takes a number"),
    };
    let built = match report::build(&mut reader, &report::Options { from_ms, to_ms, step_ms, top }) {
        Ok(r) => r,
        Err(e) => return fail(format!("couldn't read the journal for {what}: {e}")),
    };
    if args.iter().any(|a| a == "--json") {
        match serde_json::to_string_pretty(&built.only(kind)) {
            Ok(json) => println!("{json}"),
            Err(e) => return fail(e),
        }
    } else if built.intervals == 0 {
        return fail(format!("the journal for {what} has nothing recorded in that range"));
    } else {
        print!("{}", report::text(&built, kind));
    }
    std::process::ExitCode::SUCCESS
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|a| a == "report") {
        return report_main(&args[1..]);
    }
    let position = |name: &str| args.iter().position(|a| a == name);
    let value = |name: &str| position(name).and_then(|i| args.get(i + 1)).filter(|v| !v.starts_with('-')).cloned();

    let mode = if position("-r").is_some() {
        let (what, reader) = journal(value("-r"));
        let replay = reader.and_then(Replay::new);
        match replay {
            Ok(mut r) => {
                if let Some(t) = value("-b") {
                    match state::parse_time(&t) {
                        Some(at) => r.go_to_time(at),
                        None => return fail(format!("{t:?} isn't a time — HH:MM or HH:MM:SS")),
                    }
                }
                Mode::Replay(r)
            }
            Err(e) => return fail(format!("couldn't open the journal for {what}: {e}")),
        }
    } else {
        let seconds = match value("-i").map(|v| v.parse::<u64>()) {
            None => 2,
            Some(Ok(n)) if n > 0 => n,
            Some(_) => {
                eprintln!("hyprforge-top: -i takes a whole number of seconds");
                return std::process::ExitCode::FAILURE;
            }
        };
        Mode::Live(Box::new(Live::new()), Duration::from_secs(seconds))
    };

    let terminal = ratatui::init();
    let result = run(terminal, mode);
    ratatui::restore();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn key_of(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        _ => return None,
    })
}

fn run(mut terminal: ratatui::DefaultTerminal, mut mode: Mode) -> std::io::Result<()> {
    let replay = matches!(mode, Mode::Replay(_));
    let mut state = State::new(replay);
    let mut screen: Screen = match &mut mode {
        Mode::Live(live, _) => live.screen()?,
        Mode::Replay(r) => r.screen()?,
    };
    let mut next_refresh = Instant::now() + Duration::from_secs(1); // the first live screen is since boot; the next comes quickly
    loop {
        let len = render::list_len(&state, &screen);
        state.selected = state.selected.min(len.saturating_sub(1));
        terminal.draw(|f| render::draw(f, &state, &screen))?;

        let wait = match &mode {
            Mode::Live(..) if !state.paused => next_refresh.saturating_duration_since(Instant::now()),
            _ => Duration::from_secs(3600),
        };
        if event::poll(wait)? {
            let Event::Key(k) = event::read()? else { continue };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                return Ok(());
            }
            let Some(key) = key_of(k.code) else { continue };
            // The added-up views list users, programs and cgroups, not
            // processes: nothing there to end.
            let chosen = if state.view.lists_processes() { state.rows(&screen.moment).get(state.selected).map(|r| (*r).clone()) } else { None };
            let was = state.view;
            let action = state.key(key, chosen.as_ref());
            // A live-only view opened: read it now, not a refresh later.
            if let Mode::Live(live, _) = &mut mode {
                live.extras = Extras { threads: state.view == View::Threads, gpu: state.view == View::Gpu };
                if state.view != was && state.view.live_only() {
                    screen = live.screen()?;
                    next_refresh = Instant::now() + Duration::from_secs(1);
                }
            }
            match action {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Kill { pid, start } => {
                    state.message = Some(match signal::end(Id { pid, start }, How::Ask) {
                        Ok(()) => format!("Asked {pid} to end."),
                        Err(e) => format!("Couldn't end {pid}: {e}."),
                    });
                }
                Action::Interval(n) => {
                    if let Mode::Live(_, every) = &mut mode {
                        *every = Duration::from_secs(u64::from(n));
                        next_refresh = Instant::now() + *every;
                    }
                }
                Action::Step(by) => {
                    if let Mode::Replay(r) = &mut mode {
                        r.step(by);
                        screen = r.screen()?;
                    }
                }
                Action::Branch(at) => {
                    if let Mode::Replay(r) = &mut mode {
                        r.go_to_time(at);
                        screen = r.screen()?;
                    }
                }
                Action::Rewind => {
                    if let Mode::Replay(r) = &mut mode {
                        r.step(i64::MIN / 2);
                        screen = r.screen()?;
                    }
                }
            }
        } else if let Mode::Live(live, every) = &mut mode {
            screen = live.screen()?;
            next_refresh = Instant::now() + *every;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_reads_with_or_without_a_unit() {
        assert_eq!([parse_step("30s"), parse_step("10m"), parse_step("1h"), parse_step("90")], [Some(30_000), Some(600_000), Some(3_600_000), Some(90_000)]);
        assert_eq!([parse_step("0m"), parse_step("10x"), parse_step("m")], [None, None, None]);
    }
}
