//! Reusable building blocks that screens are assembled from.
//!
//! A screen that needs a search box, a text input or a hint line takes it from here instead of
//! drawing its own, so the keys and the look stay the same on every screen and a fix lands
//! everywhere at once.

pub mod command_bar;
pub mod confirm;
pub mod describe;
pub mod hints;
pub mod markdown;
pub mod panel;
pub mod search;
pub mod state_view;
pub mod table;
pub mod text_input;
pub mod tree;
