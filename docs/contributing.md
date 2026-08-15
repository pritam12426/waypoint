# Contributing

The build, test, and style setup has a few non-obvious edges. This page is
the shortcut so you don't rediscover them.

## Building

The crate is Rust 2024 edition (needs rustc ≥ 1.85). The fast path is the
dynamic-linking build, which roughly halves incremental compile time:

```
RUSTFLAGS="-C prefer-dynamic" cargo build
```

The resulting binary links `libstd` dynamically; the environment sets
`DYLD_LIBRARY_PATH` to the rustup toolchain lib dir so it finds it at
runtime. Plain `cargo check` / `cargo clippy` / `cargo test` don't use the
flag and work as usual. Run the server with:

```
cargo run -- serve
```

— no, wait, that's a lie. There is no `serve` subcommand anymore; waypointd
has no CLI at all. `cargo run` runs the server. That's the whole binary.

## The frontend build state (read this before building)

**The crate does not compile unless `frontend/dist/` exists.** The embedded
frontend uses `rust-embed`'s `#[derive(Embed)] #[folder = "frontend/dist/"]`
in `src/http/handlers/mod.rs`, which hard-errors at derive time if the
directory is missing. This applies to debug and release both. If you ever
`rm -rf frontend/dist`:

```
mkdir -p frontend/dist
```

unblocks compilation, but the `static_frontend_is_served` test stays red
until `bun run build` in `frontend/` regenerates a real build.

The frontend itself (React 19 + TypeScript, Vite, TanStack Router/Query,
Tailwind v4, Biome, bun) is real source on disk but **not committed** —
`git status` shows `frontend/` as untracked. Do not `git add` it without
asking. `frontend/dist/` and `node_modules/` are git-excluded; if the
frontend is ever committed, `dist/` must stay out.

## Testing

133 tests, split roughly 109 library unit tests (in `#[cfg(test)]` blocks
across the source) and 24 integration tests in `tests/http_api.rs`. The
integration tests don't bind a port — they build the router via
`http::app()` from an `AppState` and inject requests with
`tower::ServiceExt::oneshot`, with tempdir-backed databases, so a run
always starts from a fresh schema.

```
cargo test                 # everything
cargo test --test http_api # integration only
cargo test database::tests # one module
cargo test <test_name>     # one test
cargo clippy --all-targets
cargo fmt --check
```

Keep all four green. Clippy currently reports warnings only for the
intentional dead code in `src/logging/` (see below).

## Style

`.rustfmt.toml` sets `hard_tabs = true` and `tab_spaces = 4`. Match the
tab indentation in new code; `cargo fmt` will enforce it anyway. The
codebase is heavily commented — the comments explain _why_, and they're
expected, not noise. When you change behavior, update the doc comments that
describe it; they're the de-facto spec for the layer.

## Adding a site to the media engine

This is the designed extension point and it's a two-step change, entirely
inside `src/core/sites/`:

1. Add `src/core/sites/<site>.rs` with the URL classifiers, the extraction
   functions, and the offline rule rows. `youtube.rs` is the template —
   it's the only full example, and it demonstrates the full split:
   `ROWS` (offline rules for the `auto` mode), `is_*_url` classifiers, and
   the fetch functions with the network body-size limits and headers.
2. Register it in `src/core/sites/mod.rs`: `pub mod <site>;` plus one entry
   in `SITE_RULES` and/or `SITE_FETCHERS`. A `SITE_FETCHERS` entry
   automatically opts the site into cache-first default resolution — that's
   the whole point of the design, so don't reach into `media`/`fetch`/
   `database` to wire anything extra.

Nothing in `core::media`, `core::fetch`, or the database layer changes.
Tests for the site's URL logic live in the site module's `#[cfg(test)]`
block, next to the code.

## Changing the schema

Edit `src/database/migrations/0001_init.up.sql` — it's the single schema
file, re-run idempotently on every startup, so there's no migration version
to add and no tracking table to update. Write the change with `IF NOT
EXISTS` safety nets (the legacy-upgrade path runs fresh and legacy
databases through the same batch). If it adds a column to an existing
table, put it in the `CREATE TABLE` *and* in the guarded `ALTER` in
`migrations::init` — SQLite can't express `ADD COLUMN` idempotently.

## Things that look like bugs but aren't

- **`src/logging/` has dead code on purpose.** `next_request_id`,
  `log_set_level`/`log_get_level`, `log_use_color`, `truncate_for_log` are
  present-but-unused infrastructure. Clippy warns on them; don't delete
  them, and don't add `#[allow]`s that silence anything else.
- **Logging macros are exported from the library.** In `main.rs` (the
  binary), call them as `waypointd::log_debug!(...)`, not
  `crate::log_debug!(...)`.
- **The HTTP headers `x-total-count`, `x-next-cursor`, and
  `x-waypoint-error` are lowercase by design.** axum 0.8 doesn't normalize
  header names, and a capital-letter variant becomes a separate, ignored
  header. Keep the `HeaderName::from_static` values lowercase.
- **`database::bookmarks::count` must mirror `list` exactly.** Same filter
  → same WHERE clause. If one changes and the other doesn't, `x-total-count`
  drifts from the actual array length.
- **"Duplicate URL" is a friendly error, not a raw constraint.** `insert`
  pre-checks and bails with the "already exists as bookmark" message, and
  `http::error` special-cases that text into the 409 `conflict_url`
  contract. Keep the two in sync.
- **`utoipa`'s feature flag is `macros`, not `derive`.** The `derive`
  feature does not exist and produces a confusing compile error. Don't
  re-add the `utoipa-swagger-ui` crate either — the vendored UI shipped
  ~5 MiB of embedded assets. The interactive UI at `/api/docs` is a CDN
  HTML shell (see `src/http/docs.rs`); keep it dependency-free.
