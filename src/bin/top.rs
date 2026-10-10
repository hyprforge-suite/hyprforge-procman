//! hyprforge-top — atop for this suite, in a terminal.
//!
//! Live by default; `-r` replays the recorder's journal. Run as yourself
//! it shows what you may read and says so; under `sudo` it shows
//! everything, and a journal you may not open is asked of the helper,
//! which checks polkit first.

use hyprforge_procman::recorder;
use hyprforge_procman::sample::Id;
use hyprforge_procman::signal::{self, How};
use hyprforge_procman::tui::render;
use hyprforge_procman::tui::source::{self, Live, Replay, Screen};
use hyprforge_procman::tui::state::{Action, Key, State, View};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

const USAGE: &str = "\
hyprforge-top — what the machine is doing, atop's way

USAGE
    hyprforge-top [-i SECONDS]          live, refreshing every SECONDS (default 2)
    hyprforge-top -r [DAY|FILE] [-b HH:MM]
                                        replay the recorder's journal: a day
                                        (2026-10-10, default today) or a file,
                                        starting at a time of day

Inside, press h for every key. The journal is root's: run with sudo, or
the helper asks for your password through polkit.
";

enum Mode {
    Live(Box<Live>, Duration),
    Replay(Replay),
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }
    let position = |name: &str| args.iter().position(|a| a == name);
    let value = |name: &str| position(name).and_then(|i| args.get(i + 1)).filter(|v| !v.starts_with('-')).cloned();

    let mode = if position("-r").is_some() {
        let what = value("-r").unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
        let directory = recorder::load_config(std::path::Path::new(recorder::CONFIG_PATH)).map(|c| c.directory).unwrap_or_else(|_| recorder::Config::default().directory);
        let replay = source::open_journal(&what, &directory).and_then(Replay::new);
        match replay {
            Ok(mut r) => {
                if let Some(t) = value("-b") {
                    match t.split_once(':').and_then(|(h, m)| Some((h.parse().ok()?, m.parse().ok()?))) {
                        Some((h, m)) => r.go_to_time(h, m),
                        None => {
                            eprintln!("hyprforge-top: {t:?} isn't a time — HH:MM");
                            return std::process::ExitCode::FAILURE;
                        }
                    }
                }
                Mode::Replay(r)
            }
            Err(e) => {
                eprintln!("hyprforge-top: couldn't open the journal for {what}: {e}");
                return std::process::ExitCode::FAILURE;
            }
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
        Err(e) => {
            eprintln!("hyprforge-top: {e}");
            std::process::ExitCode::FAILURE
        }
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
            let chosen = match state.view {
                View::Users | View::Programs | View::Cgroups => None,
                _ => state.rows(&screen.moment).get(state.selected).map(|r| (*r).clone()),
            };
            match state.key(key, chosen.as_ref()) {
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
                Action::Branch { hour, minute } => {
                    if let Mode::Replay(r) = &mut mode {
                        r.go_to_time(hour, minute);
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
