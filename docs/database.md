# Database

SQLite, in WAL mode, one file. There's no database server to run alongside
waypointd, no config to point at it, and nothing to back up beyond the
single `.sqlite` file (plus, transiently, its `-wal`/`-shm` sidecars). The
default path is `waypoint.sqlite` in the working directory; `WAYPOINTD_DB_FILE`
overrides it.

## The schema init script

The schema's single source of truth is `src/database/migrations/0001_init.up.sql`,
embedded via `include_str!` and re-run on *every* startup by
`src/database/migrations.rs`. There's no versioned migration runner and no
tracking table: every statement in the script is written idempotently
(`CREATE ... IF NOT EXISTS`, `DROP ... IF EXISTS`), so re-running it on a
database that already has the schema is a no-op. Evolving the schema means
editing that one file, nothing else.

The script holds everything in one batch:

- **categories, bookmarks, tags, bookmark_tags** — the core tables. The
  bookmarks row carries `redirect_template` (TEXT, NULL):
  an optional URL template with a `{%s}` placeholder that
  `/keywords/{keyword} <value>` fills with the address-bar value
  (percent-encoded) instead of redirecting to the plain `url`.
- **Indexes** — the active-only unique indexes on `url` and `keyword` (a
  trashed bookmark never blocks re-adding the same URL or keyword), the
  ORDER BY-matching partial indexes `idx_bookmarks_created`
  (`(created_at, id) WHERE trashed_at IS NULL`), `idx_bookmarks_visit`
  (`(visit_count DESC, id ASC) WHERE trashed_at IS NULL`), and
  `idx_bookmarks_trash` (`(trashed_at, id) WHERE trashed_at IS NOT NULL`),
  plus a NOCASE partial index on `keyword` so address-bar shortcuts stay an
  index seek regardless of case. The story behind the partial indexes is
  worth reading if you touch list performance: SQLite picks exactly one
  index per query, and the old single-column `trashed_at` index had the same
  predicate as the ordering indexes, so every list request matched on
  `trashed_at = NULL` and then temp-b-tree sorted the whole corpus. Removing
  it is what lets the ordering indexes win.
- **The `updated_at` trigger** — scoped to the columns a user edit touches;
  editing the redirect template bumps the timestamp like any other
  user-touched column.
- **The two FTS5 virtual tables and their trigger set** — see below.

One thing SQLite can't express idempotently is `ALTER TABLE ... ADD COLUMN`,
so `migrations::init` adds the redirect-template column to pre-existing
`bookmarks` tables (guarded by a column check) before the script runs.

`database::open` is the only entry point. It also detects and repairs
**legacy** databases — anything with a `bookmarks` table that still carries
the old `deleted_at` recycle-bin column. Those go through `legacy_preclean`
(drop the old unguarded FTS triggers, rename `deleted_at` → `trashed_at`,
drop the dead `mime_type` column), the normal init batch, then
`legacy_postclean` (scrub trashed rows out of the main FTS index, and
rebuild both indexes if archived rows leaked into the main one). The
idempotent script is what makes one batch serve fresh and legacy databases
alike.

## Schema

The core table:

```sql
bookmarks (
    id, title, url, description, domain, category_id,
    starred, keyword, note, favicon, thumbnail,
    visit_count, last_visited_at, is_archived, trashed_at,
    redirect_template,
    created_at, updated_at,
    FOREIGN KEY (category_id) REFERENCES categories(id) ON DELETE CASCADE
)
```

The interesting decisions are about what's _not_ in a row. `trashed_at`
is the recycle bin: `NULL` = active, non-NULL = trashed. `url` and
`keyword` are unique only among active rows (partial indexes), so a trashed
bookmark never blocks re-adding the same URL or keyword — and every trash
path funnels through `trash_with_dedup`, which purges older trashed copies
of the URL first so the delete→re-add→delete cycle keeps only the newest
copy. Restore pre-checks the target URL and bails with a friendly 409
rather than letting the partial unique index throw.

`updated_at` is maintained by a trigger scoped to the columns a real edit
touches (`AFTER UPDATE OF title, url, ...`). Visit tracking writes only
`visit_count` and `last_visited_at` (`database/visits.rs`), which are
deliberately _not_ in that list — "last modified" and "last visited" are
different signals and shouldn't collapse into one timestamp. It's also what
keeps a busy keyword redirect from churning `updated_at` (and, by extension,
the FTS update triggers) on every hit.

Categories and tags are both `(id, name)` tables with unique names, linked
by `bookmark_tags (bookmark_id, tag_id)` with cascading deletes. Deleting a
category moves its bookmarks to `Uncategorized` (seeded by `database::open`,
never auto-created otherwise). There's no `visits` table — a visit is just
the two columns on the bookmark row; `database/visits.rs` owns that write
and the usage stats that read it.

## Full-text search

Two FTS5 virtual tables over the same content table:

- `bookmarks_fts` — active bookmarks only
- `bookmarks_fts_archived` — archived bookmarks only

Both use `content=bookmarks`, so the index is external-content and the
actual data isn't duplicated. A bookmark's content lives in exactly one of
three places, enforced by twelve column-scoped triggers:

- active (`trashed_at IS NULL`, `is_archived = 0`) → main index
- archived (`trashed_at IS NULL`, `is_archived = 1`) → archive index
- trashed (`trashed_at IS NOT NULL`) → neither

That makes trash and archive quarantined at the _index level_: main search
physically cannot match archived or trashed content, and archive search only
ever sees archived rows. No query-time filter can go stale because no query
needs one.

The two trigger design points worth knowing about, because they've both
bitten before:

1. Content-edit triggers carry `OLD.is_archived == NEW.is_archived` guards.
   `update --archive` rewrites every column in a single UPDATE, which fires
   the content triggers AND the archive-toggle triggers together. Without
   the guards they'd fight over a row mid-move; with them, only the toggle
   triggers act.
2. The archive-toggle triggers (`bookmarks_fts_archive` / `_unarchive`) move
   content between indexes in one shot, guarded to non-trashed rows. A
   trashed bookmark holds no index entry, so there's nothing to move there —
   the restore trigger re-adds it to whichever index its post-restore
   `is_archived` says.

Search uses `bm25()` ranking; `archived=true` on the search endpoint hits
the archive index. Because both indexes are external-content, rebuilding
them is trivial (the legacy upgrade path does exactly that with
`delete-all` + re-INSERT).

## Connections and pragmas

`database::open` returns a plain `Connection` for one-shot callers (imports,
schema init); `database::Db` is the long-lived serving shape — one writer
plus four round-robin readers, every one inside its own `Mutex`, accessed
only from `spawn_blocking`. See `architecture.md` for the reasoning; the
short version is WAL makes concurrent readers + a writer safe, and
`rusqlite::Connection` not being `Sync` is why the pool looks the way it
does.

Every connection gets the same pragmas, set in `apply_pragmas`
(`src/database/mod.rs:194`):

- `foreign_keys = true` — makes the cascading deletes actually cascade.
  Must be set outside a transaction, which is why the migration SQL doesn't
  set it.
- `busy_timeout = 5s` — a brief grace period so two processes (two `serve`
  instances) wait out a lock instead of erroring instantly.
- `journal_mode = WAL` + `synchronous = NORMAL` — readers proceed while the
  writer commits; a power loss may lose the most recent transactions but
  never corrupts the database (the WAL rebuilds from the last checkpoint).
- `cache_size = -32768` (~32 MiB page cache, tunable via
  `WAYPOINTD_DB_CACHE_SIZE`), `temp_store = MEMORY` (keeps ORDER BY / GROUP BY
  temp b-trees in RAM), `mmap_size = 256 MiB` (reads avoid page-cache
  syscalls; tunable via `WAYPOINTD_DB_MMAP_SIZE`).

On graceful shutdown the server runs `PRAGMA wal_checkpoint(TRUNCATE)` on
the writer before the pool drops, so the `-wal`/`-shm` sidecars come out
empty and get deleted by the last connection close. If you copy a database
file while the server is running, checkpoint it first (or just copy all
three files) — see `operations.md`.
