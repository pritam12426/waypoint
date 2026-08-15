/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! waypointd library crate.
//!
//! Every module lives here so the binary (`src/main.rs`) stays a thin
//! env-config shell and integration tests under `tests/` can exercise the
//! real code. Layering, bottom to top:
//!
//! * `model`    — pure data structs shared by every layer
//! * `shared`   — cross-layer helpers (URL/domain extraction, validation)
//! * `config`   — server defaults + `WAYPOINTD_*` env accessors
//! * `database` — SQLite persistence: idempotent schema init + one module per domain
//! * `core`     — business rules: media resolution (with a persistent
//!   fetch-result cache), import/export, link checker
//! * `http`     — the axum server, calls `core`/`database`
//! * `logging`  — hand-rolled structured logger (stderr or file,
//!   pretty/JSON, per-request correlation ids)
//!
//! No layer below `core` knows the HTTP layer exists, and `http` is the
//! only front door — there is no CLI to keep in sync.
//!
//! # Dependency rules
//!
//! The order of the `pub mod` declarations below is also the dependency
//! order enforced by convention:
//!
//! 1. `model`, `shared`, and `config` import nothing from the project.
//! 2. `database` imports `model` / `shared`. It also calls `core::media`
//!    (a pure, DB-free rule table) so favicon/thumbnail auto-resolution
//!    happens at the persistence choke point shared by every write path.
//! 3. `core` imports `model` / `shared` / `database`.
//! 4. `http` imports `core` + `database` + `logging` — never below `core`.

pub mod config;
pub mod core;
pub mod database;
pub mod http;
pub mod logging;
pub mod model;
pub mod shared;
