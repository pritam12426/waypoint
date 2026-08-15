<h1 align="center">
	<br>
	<img src="assets/README_icon.png" width="200">
	<br>
	📚 Waypoint
	<br>
	<br>
</h1>

A self-hosted bookmark server built around an idea that's almost too
simple: a bookmark you can _type_. Give one the keyword `ex`, and
`http://localhost:8080/keywords/ex` bounces you straight to it with a 307. No
dashboard tab to hunt for, no search box to remember — just type and go.
Everything else (tags, categories, star/archive/trash, full-text search) is
the usual bookmark-tool furniture, wrapped in a small React frontend that
gets compiled straight into the binary.

The whole server is one Rust binary. Deploying it is "copy the file
somewhere, set a few environment variables, run it." There's no config
file, no CLI, no dotenv. Configuration is entirely `WAYPOINTD_*` env vars,
and that's a feature, not an oversight: the process is meant to be launched
by something that already knows how to set environment variables.

## The server at a glance

| what                | detail                                                         |
| ------------------- | -------------------------------------------------------------- |
| What it is          | Self-hosted bookmark server with typed keyword shortcuts       |
| One binary          | API, keyword redirects, media fetcher, and SPA all in one file |
| Backend             | Rust, built on axum                                            |
| Storage             | SQLite in WAL mode — one writer, four readers                  |
| Frontend            | React 19 + TanStack Router SPA, embedded at compile time       |
| How it's configured | `WAYPOINTD_*` environment variables only (no config file)      |
| Default bind        | `localhost:8080`                                               |
| Default database    | `./waypoint.sqlite`                                            |
| HTTP API            | JSON at `/api`; OpenAPI spec at `/api/openapi.json`            |
| License             | MIT                                                            |

## Why it's built this way

Waypointd was built with a clear focus from the start, without legacy constraints.
That freedom shows up in the design—everything exists to support the core purpose:
a personal bookmark server. A few key choices kept surfacing as the ones that actually mattered:

- **One process.** The API, the keyword redirects, the media fetcher, and
  the SPA all ship in a single binary. There's nothing to keep in sync.
- **SQLite with WAL.** A bookmark collection is a single file you can just
  copy for a backup. The pool runs one writer and four readers; the
  concurrency model is spelled out in `docs/database.md`.
- **Shortcuts are first-class.** Keyword redirects are public routes served
  on purpose. A browser navigation can't attach an `Authorization` header,
  so `ex` bounces whether or not you're logged in.
- **Structured logging with correlation ids.** Every log line produced
  while handling a request carries the same `req_id`, so one request's
  whole story is a single grep. That's what the logging section in
  `docs/operations.md` shows, and it's why `WAYPOINTD_LOG_FORMAT=json`
  exists.

## Quick start

The frontend is embedded at compile time, so a full build needs the
frontend built first.

```sh
cd frontend && bun install && bun run build && cd ..
cargo build --release
```

Then run it. The defaults — `localhost:8080`, database at
`./waypoint.sqlite` — are fine for trying it out:

```sh
WAYPOINTD_SERVE_TOKEN="pick-a-long-random-string" ./target/release/waypointd
```

Open http://localhost:8080 and paste the token when the app asks. The first
start creates the database, applies the schema, and seeds an
`Uncategorized` category. SIGINT and SIGTERM both shut down gracefully: the
listener stops accepting, in-flight requests finish, and the WAL gets
checkpointed before the connections close.

For day-to-day development the server runs from `cargo run` (API on
`:8080`) and Vite serves the frontend on `:3000`, proxying `/api`,
`/keywords`, and `/open` to the backend. See `docs/contributing.md`.

## What you get

- **Keyword shortcuts** — `/keywords/{keyword}` is a 307 redirect. This is
  the whole point of the project.
- **Full-text search** — `GET /api/search` matches title, description,
  note, and URL.
- **Optional auth** — a serve token and a separate read-only token. No
  accounts, no password hashing; it's a shared-secret handshake. The SPA
  keeps the token in `localStorage` and drops you on `/settings` when the
  API answers 401.
- **Media resolution** — favicons and thumbnails are fetched and cached on
  disk (90-day TTL, capped at 10,000 entries), with site-specific rules
  where the generic ones don't work — YouTube being the notable case.
- **Keyset pagination** — list endpoints return an `x-next-cursor` (plus a
  proper `Link: rel="next"`) instead of offset math, so a page deep into a
  large collection stays O(page). The headers `x-total-count`, ETags, and
  304 handling are documented in `docs/api.md`.
- **Operational niceties** — Prometheus metrics at `/metrics`,
  `healthz`/`readyz` probes, optional automated backups, and
  `Idempotency-Key` support on mutating endpoints so a retried create
  can't silently double-save.

## Honest limits

A few things are simpler than a hosted service, on purpose, and worth
knowing before you rely on them:

- **Check jobs are in-memory.** `POST /api/check` results die with the
  process; ids restart from 1 on every boot. If a dead-link check has to
  survive a restart, run it again after.
- **Auth is token-based only.** No user accounts, no roles, no audit log.
  Two tokens, both optional, both bearer-style.
- **One database, one server.** It's built for a personal collection, not
  a multi-tenant deployment.

## Configuration

Every knob is a `WAYPOINTD_*` environment variable. There is no config
file, no CLI flags, no dotenv. Read at startup, so a change means a
restart. Here is every variable the program reads.

### Server

| variable                  | default                                       | meaning                                                                                                                                       |
| ------------------------- | --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `WAYPOINTD_DB_FILE`       | `waypoint.sqlite`                             | Where the SQLite database lives.                                                                                                              |
| `WAYPOINTD_SERVE_HOST`    | `localhost`                                   | The address to bind. `localhost` on purpose — change it only if you really want it reachable beyond this machine.                             |
| `WAYPOINTD_SERVE_PORT`    | `8080`                                        | The port to listen on.                                                                                                                        |
| `WAYPOINTD_COOKIE_SECURE` | `false`                                       | Set `true` to tag the session cookie with `Secure`. Default is fine for plain-HTTP self-hosting; set it when serving over TLS behind a proxy. |
| `WAYPOINTD_CACHE_DIR`     | platform cache dir (e.g. `~/.cache` on Linux) | Directory for the fetched-media cache (favicons/thumbnails). Falls back to the OS default when unset.                                         |

### Auth

| variable                | default   | meaning                                                                                                                  |
| ----------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------ |
| `WAYPOINTD_SERVE_TOKEN` | _(unset)_ | When set, every `/api/*` request (and the docs) needs `Authorization: Bearer <token>`. An empty value means auth is off. |
| `WAYPOINTD_READ_TOKEN`  | _(unset)_ | A second, read-only token: grants GET/HEAD only. Ignored if the serve token isn't set.                                   |

### Operations

| variable                         | default         | meaning                                                                                    |
| -------------------------------- | --------------- | ------------------------------------------------------------------------------------------ |
| `WAYPOINTD_WAL_CHECKPOINT_SECS`  | `60`            | Seconds between periodic WAL checkpoints. `0` disables the background task.                |
| `WAYPOINTD_REQUEST_TIMEOUT_SECS` | `30`            | Per-request deadline (queue wait + handler) before the server answers 504.                 |
| `WAYPOINTD_MAX_CONCURRENCY`      | `64`            | Cap on concurrently-executing API requests; saturation answers 503. Clamped to at least 1. |
| `WAYPOINTD_BACKUP_DIR`           | _(unset)_       | Directory for automated `VACUUM INTO` snapshots. Unset means no backups.                   |
| `WAYPOINTD_BACKUP_INTERVAL_SECS` | `86400` (daily) | How often automated backups run.                                                           |
| `WAYPOINTD_BACKUP_KEEP`          | `7`             | How many backups to retain before pruning the oldest. Clamped to at least 1.               |

### Logging

| variable               | default          | meaning                                                                              |
| ---------------------- | ---------------- | ------------------------------------------------------------------------------------ |
| `WAYPOINTD_LOG_LEVEL`  | `info`           | One of `off`, `fatal`, `error`, `warn`, `info`, `debug`, `trace`.                    |
| `WAYPOINTD_LOG_FORMAT` | `human-readable` | `human-readable` (colourized on a TTY) or `json` for piping into `jq`/a log shipper. |
| `WAYPOINTD_LOG_FILE`   | stderr           | Where logs go. Set a path to write to a file instead.                                |

### Frontend build-time

| variable        | default | meaning                                                                               |
| --------------- | ------- | ------------------------------------------------------------------------------------- |
| `VITE_API_BASE` | `""`    | Optional API base URL baked into the SPA at build time (defaults to the same origin). |

The full reference for the docs, headers, error codes, and the media cache
lives in `docs/` — `docs/operations.md` is the natural next read.

## Where to look

- [docs/architecture.md](docs/architecture.md) — crate layout and the threading model.
- [docs/api.md](docs/api.md) — every endpoint, auth, pagination, error codes.
- [docs/database.md](docs/database.md) — schema, full-text search, WAL.
- [docs/operations.md](docs/operations.md) — env vars, logging, media cache, backups.
- `frontend/` — the React 19 / Vite / TanStack Router SPA.

## License

[MIT](LICENSE) — see `LICENSE`.
