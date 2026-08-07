# Architecture

waypointd is a single Rust binary with a deliberately split personality: a
library crate (`src/lib.rs`) that contains every module, and a `main.rs`
that is barely a dozen lines of configuration plumbing. The library split
isn't ceremony — it exists so integration tests under `tests/` can exercise
the real code, and so nothing has to pretend to be a command-line tool. The
old project had a CLI, an HTTP server, and a shared core that all had to
agree with each other; this one has a single front door (`http`), and that
was the whole point of the rewrite.

## Module layering

The `pub mod` order in `src/lib.rs` is also the dependency order, and it's
enforced by convention rather than by any build tool:

1. `model`, `shared`, `config` — pure data structs, validation helpers, and
   env-var accessors. Nothing inside them imports the rest of the project.
2. `database` — SQLite persistence. Imports `model`/`shared`, and it also
   calls `core::media` so that favicon/thumbnail auto-resolution happens at
   the one choke point every write path shares.
3. `core` — business rules: the media engine, import/export, and the link
   checker. Imports `model`/`shared`/`database`.
4. `http` — the axum server. Imports `core` + `database` + `logging`, never
   anything below `core`.

Nothing below `core` knows the HTTP layer exists. That's what keeps the CLI
and the API from drifting apart again — there's only one consumer now, so
there's nothing to keep in sync. `logging` sits off to the side because
every layer uses it.

## A request, end to end

The interesting path starts in `src/http/mod.rs`. `app()` builds the router;
`run()` is only responsible for binding the listener and driving graceful
shutdown. Splitting the two is what lets the integration tests fire requests
at the router directly with `tower::ServiceExt::oneshot` and never touch a
socket.

Every request passes through two middlewares. `log_request` creates one
span per request carrying a monotonic `req_id` (`src/http/mod.rs:240`) — the
reason every log line from a single request carries the same id is so you
can `grep req_id=417` and get that request's whole story even when threads
interleave. It also records an info line with method, path, status, and
duration, and picks up failures that never went through the error type. The
`x-waypoint-error` response header marks responses that were already logged
by `AppError::into_response`, so nothing gets reported twice.

The `/api` sub-router carries every JSON endpoint and gets the bearer-token
middleware layered on it (`src/http/mod.rs:313`). The token gate lives on
the sub-router rather than the whole app so the address-bar redirects
(`/keywords/...`, `/open/{id}`) and the static frontend stay reachable —
a browser can't attach an `Authorization` header to a navigation.

## Threading and the connection pool

`rusqlite::Connection` is `Send` but not `Sync`, which is the constraint
that shapes everything else. `database::Db` (`src/database/mod.rs`) is a
pool of one writer plus `READ_POOL_SIZE` (4) readers, each wrapped in its
own `Mutex`. Every handler touches its connection inside
`tokio::task::spawn_blocking`, and never shares a raw connection across
tasks.

The pool shape — one writer for all mutations, round-robin readers for
list/count/search/stats — is what WAL makes safe: readers see a consistent
snapshot while the writer commits, so a page load doesn't block a visit
write. On graceful shutdown, `run()` calls `db_arc.checkpoint()` to merge
the WAL into the main file before the pool drops, so the `-wal`/`-shm`
sidecars come out empty (SQLite deletes them when the last connection
closes).

There's a second pool of threads that follows the same discipline for the
opposite reason. The link checker (`src/core/checker.rs`) runs probe
workers that touch SQLite exactly never — the `Connection` stays on the
calling thread, and results flow back over `mpsc` channels to a single
aggregator. Both designs are the same instinct: never share a connection,
and state exactly where the one connection lives.

## Media resolution

The media engine lives in `src/core/sites/mod.rs` and is two tables, both
first-match-wins:

- `SITE_RULES` — offline rules keyed by URL, used by the `auto` asset mode
  (e.g. "YouTube video URL → this is a video page, skip the generic
  domain-icon fallback").
- `SITE_FETCHERS` — network `matches` + `fetch` function-pointer entries,
  each tagged with `MediaTarget::Favicon` or `Thumbnail`. This is what
  backs `AssetMode::Fetch`.

`core::media` dispatches through both, and the targets are independent — a
favicon and a thumbnail for the same URL resolve on their own. All
YouTube-specific logic (offline rules, avatar extraction, URL classifiers,
tests) lives in `src/core/sites/youtube.rs`. Adding a site is one new module
plus table entries; nothing in `media`/`fetch`/`database` changes.

Default resolution is cache-first for any URL with a matching fetcher.
`resolve_favicon`/`resolve_thumbnail` run the network pipeline whenever a
fetcher matches, and store successful results in the persistent cache
(`src/core/cache.rs`): a JSON file at `<cache_dir>/waypoint/media-cache.json`
keyed by bookmark URL + target, 90-day TTL, capped at 10,000 entries, atomic
writes. Only successes are cached — a `None` is retried next time, and the
offline rule-table fallback is never cached (it's free, and this keeps
rule-table edits visible immediately instead of pinned by a stale entry).
`PUT /api/bookmarks/{id}` with `refresh: true` bypasses the read for one
bookmark and rewrites the cache.

## Background jobs and the caches

`AppState` carries three things beyond the pool. `Jobs` (`src/http/jobs.rs`)
is the in-memory registry for check runs — ids are monotonic per process,
finished jobs are reaped after an hour. This is deliberately not
persistent: a check run is a transient batch, and a crash just loses it.

`CountCache` and `StatsCache` (`src/http/cache.rs`) are the opposite trade.
They're in-memory, shared across handlers. Each entry carries the closure
that recomputes it, so a successful write _refreshes_ the warm entries in
place (the RAM cache reflects the new data immediately and the next read is
still a hit). The async visit-tracking redirects (which run fire-and-forget
outside a write transaction) and any cache-refresh failure fall back to
invalidating them wholesale. On top of that, the stats and tags endpoints send
`Cache-Control: private, max-age=30` plus a strong ETag and answer 304 when
the client's `If-None-Match` matches. Two cache layers with two different
lifetimes, and neither ever has to reason about staleness of individual
keys.
