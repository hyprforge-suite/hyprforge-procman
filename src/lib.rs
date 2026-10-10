//! hyprforge-procman — what is running, what it costs, and which window
//! it belongs to.
//!
//! The layers, bottom up: [`proc`] reads `/proc`; [`windows`] asks
//! Hyprland which process owns each window; [`sample`] turns two readings
//! into use; [`model`] groups, sorts and searches, as plain data; and
//! [`signal`] is the one thing that changes anything — ending a process,
//! never one that merely reused its number. The window is `main.rs`.

pub mod graph;
pub mod helper;
pub mod history;
pub mod machine;
pub mod model;
pub mod proc;
pub mod sample;
pub mod services;
pub mod signal;
pub mod users;
pub mod windows;
