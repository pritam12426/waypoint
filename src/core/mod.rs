/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Business rules shared by the CLI and HTTP layers.
//!
//! * `cache`   — persistent cache of fetched-media results (favicon/
//!   thumbnail URLs), keyed by bookmark URL + `MediaTarget`
//! * `media`   — dispatcher for favicon/thumbnail resolution
//! * `sites`   — per-site offline rules (`SITE_RULES`) + network fetchers
//!   (`SITE_FETCHERS`, target-scoped by `MediaTarget`)
//! * `fetch`   — generic, site-agnostic network scrape engine
//! * `url`     — URL sanity/format helpers used by both front doors
//! * `import_export` — HTML/Markdown/CSV import and export
//! * `checker` — link-liveness checker
//!
//! The CLI and server both call into this module so rules live in exactly
//! one place.
//!
//! # Why this module exists
//!
//! Without a shared `core`, the CLI and HTTP layers would each re-implement
//! import/export, the checker, and media resolution, and the two front
//! doors would drift. This is the layer `cmd` and `http` are *allowed* to
//! depend on together (besides `database`), which keeps a behavior fixed
//! here automatically correct in both places.
//!
//! Dependency note: `core` sits above `database` and `shared`, so its
//! modules import them freely — but nothing in `core` knows about `cmd`,
//! `http`, or the async runtime.

pub mod cache;
pub mod checker;
pub mod fetch;
pub mod import_export;
pub mod media;
pub mod sites;
pub mod url;
