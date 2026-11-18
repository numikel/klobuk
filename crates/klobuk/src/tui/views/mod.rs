//! One module per view. Every module has the same four parts, so the view tasks touch disjoint files:
//! `State` (selection, scroll, filter, folds), `render`, `handle_key` and `BINDINGS`.

pub mod agents;
pub mod board;
pub mod config;
pub mod inbox;
pub mod log;
pub mod now;
pub mod queue;
pub mod steps;
