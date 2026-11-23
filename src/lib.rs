//! ouija: ask a System One decision model typed questions from the shell.
//!
//! The binary lives in `main.rs`. The modules here hold the pure logic so it
//! can be unit-tested without a server.

pub mod answer;
pub mod config;
pub mod flavor;
pub mod spec;
