//! hyprforge-procman — what is running, what it costs, and which window
//! it belongs to.
//!
//! The layers, bottom up: [`proc`] reads `/proc`; [`windows`] asks
//! Hyprland which process owns each window; [`sample`] turns two readings
//! into use; [`model`] groups, sorts and searches, as plain data; and
//! [`signal`] is the one thing that changes anything — ending a process,
//! never one that merely reused its number. The window is `main.rs`.

pub mod app_history;
pub mod cgroups;
pub mod exits;
pub mod gpu;
#[cfg(feature = "gui")]
pub mod graph;
#[cfg(feature = "dbus")]
pub mod helper;
pub mod inspect;
pub mod journal;
pub mod history;
pub mod machine;
pub mod model;
pub mod moment;
pub mod nvidia;
pub mod proc;
pub mod recorder;
pub mod sample;
#[cfg(feature = "dbus")]
pub mod services;
#[cfg(feature = "dbus")]
pub mod sessions;
pub mod signal;
#[cfg(feature = "desktop")]
pub mod startup;
#[cfg(feature = "tui")]
pub mod tui;
pub mod users;
pub mod windows;
