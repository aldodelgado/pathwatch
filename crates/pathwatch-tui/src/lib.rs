//! Terminal dashboard for `pathwatch` (placeholder name).
//!
//! This crate draws [`pathwatch_core::snapshot::Snapshot`]s and turns key
//! presses into a [`pathwatch_core::snapshot::ViewSpec`]. It cannot reach the
//! network or a probe timer: its only inputs are snapshots, the keyboard and,
//! when asked for, the mouse.

pub mod app;
pub mod mouse;
pub mod theme;
pub mod ui;
pub mod widgets;
