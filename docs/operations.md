# Operations

waypointd is configured entirely by environment variables — there are no CLI
flags, no config file, no dotenv. The defaults are chosen for a personal,
localhost-bound tool, and every one of them can be overridden.

## Environment variables

| variable                | default            | meaning                                                                       |
| ----------------------- | ------------------ | ----------------------------------------------------------------------------- |
| `WAYPOINTD_DB_FILE`     | `waypoint.sqlite`  | SQLite database path                                                          |
| `WAYPOINTD_SERVE_HOST`  | `localhost`        | bind host (IPv4/IPv6 address or a hostname)                                   |
| `WAYPOINTD_SERVE_PORT`  | `8080`             | bind port (must be ≥ 1)                                                       |
| `WAYPOINTD_SERVE_TOKEN` | _(none)_           | if set, `/api/*` + docs require `Authorization: Bearer <token>`               |
| `WAYPOINTD_CACHE_DIR`   | platform cache dir | where the fetched-media cache lives                                           |
| `WAYPOINTD_LOG_LEVEL`   | `info`             | `error` / `warn` / `info` / `debug` / `trace` / `off` (`fatal` also accepted) |
| `WAYPOINTD_LOG_FORMAT`  | `human-readable`   | `human-readable` or `json`                                                    |
| `WAYPOINTD_LOG_FILE`    | _(stderr)_         | append logs to this file instead of stderr                                    |

The bind host is validated before the database is even opened, so a typo
fails fast instead of leaving a half-initialized server around. `localhost`
is the default on purpose — the server is only reachable from this machine
until you ask for more. Port 0 is rejected for the same reason (a real port
is the only sensible outcome here).

## Running

```
WAYPOINTD_SERVE_TOKEN="some-long-random-string" \
WAYPOINTD_DB_FILE="$HOME/.waypoint/waypoint.sqlite" \
  ./waypointd
```

The first start creates the database, applies the migrations, and seeds the
`Uncategorized` category. The server prints `waypointd listening on
http://localhost:8080` and logs a request span per request. SIGINT and
SIGTERM both trigger a graceful shutdown — the listener stops accepting,
in-flight requests finish, and the WAL is checkpointed before the
connections close.

If you set a token, the frontend needs it too. The browser can't send
`Authorization` on a navigation, so the SPA stores the token in
`localStorage` and attaches it to every API call; when a request comes back
401, it clears the token and sends you to `/settings` to paste it again.
There's no login flow or password hashing anywhere — the token is a
shared-secret handshake, and the address-bar redirect routes
(`/keywords/...`, `/open/{id}`) stay open regardless because a navigation
can't send the header.

## Logging

The human-readable format looks like this:

```
[12-Aug-2026 09:04:55.446625] [INFO ] http{method=POST path=/api/bookmarks req_id=0}: waypointd::http::handlers: [::1]:57439 created bookmark #1: https://example.com/one
```

The `req_id` is the request's whole story in one grep: `req_id=417` finds
every log line one request produced, even when threads interleave. Level
labels are aligned (`[INFO ]`, `[WARN ]`, ...) and colored only when stderr
is a terminal — never when logging to a file.

The JSON format is flattened and compact, one object per line:

```json
{"ts":"12-Aug-2026 09:04:55.465109","level":"info","msg":"created bookmark #1: https://example.com/one","target":"waypointd::http::handlers","method":"POST","path":"/api/bookmarks","req_id":0}
```

`ts` is local time in the same `%d-%b-%Y %H:%M:%S%.6f` shape as the
human form; span fields (`method`, `path`, `req_id`) are merged into the
object. This format is what you want if you're shipping the logs to a
collector.

Every request logs a completion line at `info` with method, path, status,
and duration — the request-level access log — plus startup/shutdown,
request failures (4xx as warnings, 5xx as errors), and anything a handler
logs directly. `debug`/`trace` add finer detail (e.g. the exact SQLite
connection serving a query).

## The media cache

Bookmark saves and URL changes re-fetch the same favicons and thumbnails
over and over. Successful results are cached on disk at
`<cache_dir>/waypoint/media-cache.json`, where `cache_dir` is
`WAYPOINTD_CACHE_DIR` if set, else the platform cache directory
(`~/.cache` on Linux, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on
Windows), else the temp dir.

Facts you might need someday:

- 90-day TTL, capped at 10,000 entries across both favicons and thumbnails
  (oldest dropped).
- Only _successful_ network results are cached. A page being down is never
  cached, so it's retried next time.
- Writes are atomic (temp file + rename), and a corrupt or
  version-mismatched file is logged and treated as empty — a bad cache file
  costs you one re-fetch, never a crash.
- `refresh: true` on `PUT /api/bookmarks/{id}` bypasses the read for that
  bookmark and rewrites the cache with the fresh result.
- Deleting the file is always safe. The worst case is a slow first save.

## Background jobs

Check jobs (`POST /api/check`) live in process memory only. They're
transient batch runs: ids restart from 1 on every boot, finished jobs are
reaped after an hour, and a crashed server simply loses in-flight checks.
If you need a check to survive a restart, run it again after.

## Backing up

The database is a single SQLite file, so a backup is a copy — with one
caveat. WAL means the live database has `-wal` and `-shm` sidecars; copying
just the main file while the server is running can miss the most recent
commits. The clean way to take a hot backup:

```
sqlite3 waypoint.sqlite "PRAGMA wal_checkpoint(TRUNCATE);"
cp waypoint.sqlite backup.sqlite
```

or stop the server first (graceful shutdown checkpoints the WAL
automatically). The `-wal`/`-shm` files are transient and must never be
committed or backed up on their own. The media cache in
`~/.cache/waypoint/` is a cache — it can be copied for warmth or omitted
entirely.

## Sizing and resource notes

A release build is a single stripped binary, small enough to forget about
(size optimization is a stated goal — `opt-level = "s"`, fat LTO, strip;
see `Cargo.toml`). At runtime SQLite maps up to 256 MiB of the file read-only
and holds ~32 MiB of page cache. The one number that actually scales with
your collection is the media cache, which is bounded at 10,000 entries. A
list page beyond a few thousand bookmarks is where the keyset cursor in
`api.md` earns its keep — offset pagination on the default `created_at`
ordering works, but the cursor stays O(page) instead of scanning past
everything you've already seen.
