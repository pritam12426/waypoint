/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! HTTP request handlers: one `pub async fn` per endpoint, plus the static
//! frontend fallback. Three patterns repeat throughout:
//!
//! * **Blocking DB work is off the async runtime** — each handler clones the
//!   `Arc<Mutex<Connection>>` and runs the query inside
//!   `tokio::task::spawn_blocking`. The `Mutex` guarantees only one task
//!   touches the connection at a time (SQLite connections aren't `Sync`).
//! * **Validation happens before the DB call** — id/limit/offset/keyword
//!   checks return `AppError` early so bad input never reaches SQL.
//! * **Client address is logged** — via the `ConnectInfo<SocketAddr>`
//!   extractor, enabled by `into_make_service_with_connect_info` in `run`.
//!
//! Handlers are grouped by concern into sibling modules and re-exported
//! here, so `http::handlers::<fn>` stays the single public path for the
//! router and the OpenAPI generator. `shared` holds the helpers the groups
//! have in common (validation, the cached-JSON/ETag pipeline); it is *not*
//! re-exported.

mod admin;
mod bookmarks;
mod catalog;
mod check;
mod frontend;
mod import_export;
mod keywords;
mod ops;
mod session;
mod shared;
mod stats;

pub use admin::*;
pub use bookmarks::*;
pub use catalog::*;
pub use check::*;
pub use frontend::*;
pub use import_export::*;
pub use keywords::*;
pub use ops::*;
pub use session::*;
pub use stats::*;
