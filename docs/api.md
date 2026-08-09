# HTTP API

Everything JSON lives under `/api`. The server is a personal tool by
default: it binds `localhost:8080` and serves every endpoint with no
authentication until you set `WAYPOINTD_SERVE_TOKEN`. A machine-readable
version of this page exists at `/api/openapi.json` (behind the same token
gate as the API itself). An interactive Swagger UI is served at `/api/docs`
— a small HTML shell that loads swagger-ui-dist from a CDN, so nothing is
embedded in the release binary and the page needs internet access to
render.

All timestamps in responses are `YYYY-MM-DD HH:MM:SS` in UTC, matching the
raw SQLite values. Request bodies and response bodies are JSON.

## Authentication

When `WAYPOINTD_SERVE_TOKEN` is set, every `/api/*` endpoint and
`/api/openapi.json` require:

```
Authorization: Bearer <token>
```

The comparison is constant-time (`subtle`), and a missing or wrong token is
a 401 with `WWW-Authenticate: Bearer` on the response. The frontend reads
that header to know when it should prompt for a token.

These are **never** gated, because a browser navigation can't send the
header:

- `GET /keywords` — plain-text keyword list
- `GET /keywords/{keyword}` — 307 redirect to the bookmark
- `GET /open/{id}` — 307 redirect by id
- the static frontend fallback (everything else)

## Errors

Every error response has the JSON shape:

```json
{ "error": "human-readable message", "code": "machine_code" }
```

and sets the `x-waypoint-error` header to the same code (handlers use it to
avoid double-logging). The codes are stable enough to branch on, but don't
treat them as a public contract — they're what the frontend and the tests
happen to use.

| code               | status | meaning                                                                                         |
| ------------------ | ------ | ----------------------------------------------------------------------------------------------- |
| `invalid_url`      | 400    | missing/empty/blank URL                                                                         |
| `invalid_keyword`  | 400    | keyword has characters outside `[A-Za-z0-9._-]`                                                 |
| `invalid_limit`    | 400    | limit out of range, or a bad cursor/bulk-delete combo                                           |
| `invalid_offset`   | 400    | negative offset                                                                                 |
| `invalid_id`       | 400    | id not a positive integer                                                                       |
| `invalid_name`     | 400    | empty category/tag name, or mutating the default category                                       |
| `invalid_date`     | 400    | date filter doesn't parse, or after-before inverted                                             |
| `invalid_payload`  | 400    | malformed body / empty import / bad export format                                               |
| `query_required`   | 400    | search `q` missing                                                                              |
| `unauthorized`     | 401    | bad/missing bearer token                                                                        |
| `not_found`        | 404    | bookmark/category/tag/job missing                                                               |
| `conflict_url`     | 409    | URL already exists as an active bookmark                                                        |
| `conflict_keyword` | 409    | keyword already in use by another active bookmark                                               |
| `internal_error`   | 500    | never leaks details — message is always "internal server error", the real cause goes to the log |

409s and 500s are classified from the underlying error message inside
`src/http/error.rs`; the blanket rule is: message mentions "already exists
as bookmark" → `conflict_url`, "already in use by bookmark" →
`conflict_keyword`, and a raw SQLite unique-constraint violation is split by
whether it names the `keyword` column.

## The Bookmark object

List, search, create, update, and the per-bookmark stats endpoint all return
this shape:

```json
{
  "id": 1,
  "title": "Example",
  "url": "https://example.com/",
  "description": null,
  "domain": "example.com",
  "category_id": 1,
  "category_name": "Uncategorized",
  "starred": false,
  "keyword": "ex",
  "note": null,
  "favicon": null,
  "thumbnail": null,
  "visit_count": 3,
  "last_visited_at": null,
  "is_archived": false,
  "created_at": "2026-08-01 10:00:00",
  "updated_at": "2026-08-01 10:00:00",
  "trashed_at": null,
  "tags": ["one", "two"]
}
```

Two fields need a heads-up. `favicon` may hold the sentinel value
`"\0default-favicon"` (and `thumbnail` its `"\0default-thumbnail"` twin) —
a NUL-prefixed token meaning "no custom asset, show the generic domain
icon". It's a string, not null, so a client can't tell "unset" from "no
icon" just by `== null`. And `trashed_at` being non-null is what defines "in
the recycle bin"; such bookmarks are excluded from most queries and can only
be listed with `trash=true`.

## Pagination

Two mechanisms, and they don't mix.

**Offset** (the default): `limit` + `offset`. The list default is 200, search
50, `limit` is clamped to 1–1000. `offset` must be ≥ 0.

**Cursor** (list only): pass `cursor` (the value of `x-next-cursor` from the
previous response) and `offset` is ignored. The cursor is a keyset bound on
`(created_at, id)`, the exact columns the list's `ORDER BY created_at DESC`
walks, so deep pages stay O(page) instead of skipping through an offset
scan. It only exists on the _active_ list — the trash view never emits one,
and passing one to `trash=true` is a 400.

Two response headers matter, and both are lowercase (axum 0.8 does not
normalize header names, so a capital-letter variant silently becomes a
second, ignored header):

- `x-total-count` — total matches for the query, ignoring pagination.
- `x-next-cursor` — present only when the page is full (`len == limit`);
  a short page is the last one, so the header is omitted.

## Bookmarks

### `GET /api/bookmarks` — list

Query parameters are all optional; absent means "don't filter". Filters
combine with AND.

| param                              | type   | notes                                                                                          |
| ---------------------------------- | ------ | ---------------------------------------------------------------------------------------------- |
| `category`                         | string | by category name                                                                               |
| `category_id`                      | int    | by category id                                                                                 |
| `tag`                              | string | by tag name                                                                                    |
| `keyword`                          | string | by keyword                                                                                     |
| `starred`                          | bool   |                                                                                                |
| `archived`                         | bool   | `true` = only archived, `false` = only active                                                  |
| `trash`                            | bool   | lists trashed bookmarks; overrides `archived`                                                  |
| `created_after` / `created_before` | string | UTC `YYYY-MM-DD[ HH:MM[:SS]]`; a bare date means day-start (`*_after`) or day-end (`*_before`) |
| `updated_after` / `updated_before` | string | same                                                                                           |
| `visited_after` / `visited_before` | string | same, against `last_visited_at`                                                                |
| `trashed_after` / `trashed_before` | string | same, only meaningful with `trash=true`                                                        |
| `limit`, `offset`                  | int    | see pagination                                                                                 |
| `cursor`                           | string | see pagination                                                                                 |

Sorting: `created_at DESC` for active, `trashed_at DESC` for trash.

### `POST /api/bookmarks` — create

Body (`NewBookmark`); only `url` is required:

```json
{
  "url": "https://example.com/",
  "title": null,
  "description": null,
  "category": "Uncategorized",
  "tags": [],
  "keyword": null,
  "note": null,
  "favicon": null,
  "thumbnail": null,
  "favicon_mode": "auto",
  "thumbnail_mode": "auto",
  "starred": false,
  "is_archived": false
}
```

- `title` defaults to the URL. `category` defaults to `Uncategorized`.
- `keyword` of `""` or null means "no keyword".
- `favicon`/`thumbnail`: null = auto-resolve; `favicon: ""` forces the
  generic domain favicon; `thumbnail: ""` stores none.
- `favicon_mode`/`thumbnail_mode` are `"auto"` (default) | `"default"` |
  `"fetch"` and win over the explicit fields when set. `auto` resolves
  offline first and falls back to a network fetch only when a site fetcher
  matches; `fetch` always goes to the network; `default` always uses the
  generic icon.
- `is_archived` defaults to false.

Returns 201 with the hydrated `Bookmark` (tags and category resolved). A
duplicate active URL is 409 `conflict_url`; a duplicate keyword is 409
`conflict_keyword`.

### `GET /api/bookmarks/{id}` — get

200 + `Bookmark`, or 404.

### `PUT /api/bookmarks/{id}` — update

Tri-state semantics: `null` or absent field = leave unchanged. `tags` is a
full replacement (empty array clears); `add_tags`/`remove_tags` patch
incrementally. `keyword: ""` clears it. `refresh: true` re-fetches the
favicon/thumbnail for this bookmark, bypassing the 90-day media cache (and
rewriting it with the fresh result). `url` of `""` is a 400. Returns 200 +
`Bookmark`; 404 if the id is gone; 409 on URL/keyword collisions.

### `DELETE /api/bookmarks/{id}` — delete

`?purge=true` permanently deletes; the default moves the bookmark to the
trash. Returns 204, or 404.

### `POST /api/bookmarks/{id}/restore` — restore

Pulls a bookmark out of the trash. 204 on success, 404 if missing, 409 if
restoring would collide with a live URL.

### `PATCH /api/bookmarks` — bulk update

```json
{ "ids": [1, 2, 3], "update": { "starred": true } }
```

`update` uses the same tri-state shape as the single update, but must
contain at least one actual change. Returns:

```json
{ "updated": [1, 2], "skipped": [3] }
```

`skipped` is ids that are missing or trashed. Validation happens up front
(an invalid payload writes nothing); a write-time collision aborts mid-batch
with a 409.

### `DELETE /api/bookmarks` — bulk delete

Query-only, and it takes either an `ids=` comma-separated list **or** the
filter criteria from the list endpoint — both or neither is a 400 (the
catch-all refusal is intentional). `dry_run=true` returns the ids that
_would_ be removed without removing them. Response:

```json
{ "ids": [4, 5], "removed": 2 }
```

### `DELETE /api/trash` — empty trash

`?before=YYYY-MM-DD` limits the purge to bookmarks trashed at or before that
time; `?dry_run=true` previews. Response is the same `{ ids, removed }`
shape.

## Categories and tags

- `GET /api/categories` → array of `{ "id", "name" }`.
- `PUT /api/categories/{id}` body `{ "name": "..." }` — renaming the default
  category is a 400.
- `DELETE /api/categories/{id}` → 204; the category's bookmarks move to the
  default category.
- `GET /api/tags` → array of `{ "name", "count" }` (cached, see below).
- `PUT /api/tags/{name}` body `{ "name": "..." }` → 200.
- `DELETE /api/tags/{name}` → 204; associations are dropped.

`GET /api/tags` and every `/api/stats*` response carry
`Cache-Control: private, max-age=30` and a strong `ETag`, and answer 304
when `If-None-Match` matches.

## Search

`GET /api/search?q=...` — matches title, description, note, and URL, using
FTS5 ranking (best matches first). Supports `category`, `tag`, `keyword`
narrowing, `limit` (default 50, max 1000), and `archived` (default false;
searches the separate archive index). Returns a bare array of `Bookmark`
plus `x-total-count`. There is no offset or cursor on search — for deep
paging you're expected to narrow the query.

## Import and export

### `POST /api/import`

Body (`camelCase`):

```json
{ "content": "<netscape html>", "tags": ["bulk"], "category": null, "archive": false }
```

Parses a Netscape/HTML bookmark file (every major browser's export format).
`<H3>` folder headings become categories; a bookmark ends up tagged with the
folder it appeared under. `tags` adds tags to every imported bookmark,
`category` overrides the folder headings for all of them, and `archive`
sends everything to the archive. Duplicate URLs are skipped, not
double-created — imports funnel through the same `insert` as the API.

Response:

```json
{ "imported": 41, "skipped": 3 }
```

### `GET /api/export`

`?format=md` (default) or `?format=csv`. The response body is the raw
document text (`Content-Type: text/markdown` or `text/csv`) — no JSON
wrapper, no file download:

```text
# Bookmarks

## Uncategorized
- [Example](https://example.com/) `ex` #dev
  the description
```

Markdown groups by category; CSV is flat with the header
`id,title,url,description,category,tags,keyword,note,favicon,starred`. Only
**active** bookmarks are exported — trashed and archived stay out of backups
by default.

## Dead-link check

### `POST /api/check`

```json
{ "delete": false, "hardDelete": false, "jobs": 4 }
```

`delete` moves dead links to the trash; `hardDelete` purges them; both at
once is a 400. `jobs` is worker threads (defaults to the CPU count, clamped
≥ 1). The run happens in the background. Returns **202**:

```json
{ "id": 1 }
```

### `GET /api/check/{id}`

Polls the job. The response is a tagged enum:

```json
{ "status": "running", "checked": 30, "total": 41, "dead": 1 }
```

```json
{
  "status": "finished",
  "checked": 41,
  "alive": 39,
  "skipped": 1,
  "deleted": 1,
  "dead": [{ "id": 5, "title": "...", "url": "...", "reason": "HTTP 404" }]
}
```

```json
{ "status": "failed", "error": "..." }
```

### `GET /api/bookmarks/{id}/check`

Synchronous single-link check — no job to poll. Probes just that bookmark
and returns its verdict directly (up to the same 10-second probe budget):

```json
{ "status": "alive" }
```

```json
{ "status": "dead", "reason": "HTTP 404" }
```

```json
{ "status": "skipped" }
```

`skipped` covers non-http(s) URLs. A nonexistent or **trashed** bookmark is
a 404 — like the batch job, this only checks active bookmarks.

Liveness: any 2xx/3xx counts as alive (redirects are followed). HEAD is
tried first, and a HEAD rejected with 405/501 is retried with GET — HEAD is
optional per RFC 7231. Timeouts, DNS failures, connection errors, and
4xx/5xx are all dead. Non-http(s) URLs (`mailto:` etc.) are counted in
`skipped` and never probed. Each probe has a fixed 10-second budget.
`reason` is `HTTP <code>`, `timed out`, or the underlying error text.

Probes share the media fetch engine's SSRF guard: loopback, private,
link-local, and unique-local destinations are refused outright (reported
dead) so a saved URL can never make the checker reach internal hosts. This
also means a bookmark pointing at a LAN address like `192.168.x.x` or
`127.0.0.1` is always dead.

Jobs live only in process memory: ids are monotonic from 1, a poll on a
missing id is 404, and finished jobs are reaped after an hour. Restart the
server and in-flight checks are simply gone.

## Stats

All paged stats endpoints take `limit` + `offset`; defaults are in the
table. They're all cached 30s as described above.

| endpoint                        | default limit | returns                                                                                               |
| ------------------------------- | ------------- | ----------------------------------------------------------------------------------------------------- |
| `GET /api/stats`                | —             | overview (below)                                                                                      |
| `GET /api/stats/domains`        | 50            | `{ "domain", "count" }[]`                                                                             |
| `GET /api/stats/tags`           | 50            | `{ "name", "count" }[]`                                                                               |
| `GET /api/stats/bookmarks/{id}` | —             | a single `Bookmark`                                                                                   |
| `GET /api/stats/top-visited`    | 20            | `{ "domain", "total_visits", "bookmark_count" }[]`                                                    |
| `GET /api/stats/never-visited`  | 50            | `{ "id", "title", "url", "domain", "created_at" }[]`                                                  |
| `GET /api/stats/orphan-tags`    | 50            | `{ "name", "bookmark_id", "bookmark_title" }[]` (tags that exist only because a bookmark was deleted) |
| `GET /api/stats/hygiene`        | —             | `{ "total", "missing_tags", "missing_note", "missing_description" }`                                  |
| `GET /api/stats/activity`       | 12            | `{ "month": "YYYY-MM", "count" }[]`, most recent first                                                |

The overview:

```json
{
  "total": 12, "starred": 3, "archived": 1, "trashed": 2,
  "categories":  [{ "name": "Uncategorized", "count": 9 }],
  "top_domains": [{ "domain": "example.com", "count": 4 }],
  "top_tags":    [{ "name": "dev", "count": 5 }],
  "most_visited": [ { "id": 1, "title": "...", "url": "...", "domain": "example.com",
                       "visit_count": 21, "last_visited_at": "...", "created_at": "..." } ],
  "recently_added": [ /* same shape */ ]
}
```

