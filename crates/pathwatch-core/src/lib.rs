//! Measurement core for `pathwatch` (placeholder name).
//!
//! This crate has no terminal dependencies: a renderer can only consume what
//! is published here and cannot influence probe timing.

pub mod capability;
pub mod clock;
pub mod collector;
pub mod config;
pub mod correlate;
pub mod discovery;
pub mod engine;
pub mod glossary;
pub mod incident;
pub mod localnet;
pub mod model;
pub mod netwatch;
pub mod platform;
pub mod probe;
pub mod processor;
pub mod recorder;
pub mod sanitize;
pub mod schedule;
pub mod script;
pub mod snapshot;
pub mod stats;
#[doc(hidden)]
pub mod testing;
pub mod trace;
pub mod traffic;
