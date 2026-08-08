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

