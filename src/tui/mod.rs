//! `hyprforge-top`: atop's screen, live or replayed from the recorder's
//! journal. [`state`] is what a key does, [`source`] where a screen comes
//! from, [`render`] how it is drawn — the binary is only the loop.

pub mod render;
pub mod source;
pub mod state;
