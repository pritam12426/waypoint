/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Shared defaults and constants for the CLI and server layers, so one
//! value can't drift between `cmd` and `http`.
//!
//! # Why this file exists
//!
//! The CLI (`src/cmd/`) and the HTTP server (`src/http/`) each expose
//! configuration via flags and environment variables (e.g. `--host`,
//! `WAYPOINT_DB_FILE`). If each layer hard-coded its own defaults, the two
//! front doors could silently disagree about things like the default port
//! or the default database filename. Every default that matters to more
//! than one layer is defined exactly once here, and the layers reference
//! these constants instead of literals.
//!
//! # Notes on usage
//!
//! * `DEFAULT_DB_FILE` is also the clap `default_value` for the global
//!   `--database` flag, so bare `waypoint` invocations use it implicitly.
//! * `DEFAULT_LIST_LIMIT` / `DEFAULT_SEARCH_LIMIT` describe the *intended*
//!   page sizes. Note that the HTTP handlers currently default to 200 (list)
//!   and 50 (search) inline — if those are ever unified, this is the single
//!   source of truth to keep in sync.
//! * `DEFAULT_PORT` and `DEFAULT_HOST` are the `serve` defaults, mirrored by
//!   the `WAYPOINT_SERVE_HOST` / `WAYPOINT_SERVE_PORT` env vars.

/// Default SQLite database file (used by the global `--database` flag).
pub const DEFAULT_DB_FILE: &str = "waypoint.sqlite";

/// Default host the `serve` command binds to (`localhost` — deliberately
/// not `0.0.0.0`, so the server is only reachable on this machine unless
/// the user asks for more).
pub const DEFAULT_HOST: &str = "localhost";

/// Default port the `serve` command listens on.
pub const DEFAULT_PORT: u16 = 8080;

/// Default page size for `list` (CLI and HTTP default when no `limit`).
pub const DEFAULT_LIST_LIMIT: i64 = 50;

/// Default page size for `search`.
pub const DEFAULT_SEARCH_LIMIT: i64 = 20;
