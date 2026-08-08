/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! waypoint library crate.
//!
//! Every module lives here so the binary (`src/main.rs`) stays a thin CLI
//! shell and integration tests under `tests/` can exercise the real code.
//! Layering, bottom to top:
//!
//! * `model`   — pure data structs shared by every layer
//! * `shared`  — cross-layer helpers (URL/domain extraction, validation)
//! * `database`— SQLite persistence: migrations + one module per domain
//! * `core`    — business rules: media resolution (with a persistent
//!   fetch-result cache), import/export, checker
//! * `cmd`     — CLI layer (clap), calls `core`/`database`
//! * `http`    — axum layer, calls `core`/`database`
//! * `logging` — hand-rolled structured logger (stderr or file, pretty/JSON)
//!
//! No layer below `core` knows the CLI or HTTP exist, and `cmd`/`http`
//! share `core` — that single shared seam is what keeps behavior identical
//! across both front doors without duplicating rules.
//!
//! # Dependency rules
//!
//! The order of the `pub mod` declarations below is also the dependency
//! order enforced by convention:
//!
//! 1. `model` and `shared` import nothing from the project.
//! 2. `database` imports `model` / `shared`. It also calls `core::media`
//!    (a pure, DB-free rule table) so favicon/thumbnail auto-resolution
//!    happens at the persistence choke point shared by every write path.
//! 3. `core` imports `model` / `shared` / `database`.
//! 4. `cmd` and `http` import `core` + `database` + `logging` — never each
//!    other, and never below `core`.
//!
//! Keeping this one-way is what guarantees a behavior fixed in one layer
//! (say, a validation rule in `shared` or a query in `database`) is
//! automatically shared by the CLI and the HTTP API.

pub mod cmd;
pub mod config;
pub mod core;
pub mod database;
pub mod http;
pub mod logging;
pub mod model;
pub mod shared;
