# Database

SQLite, in WAL mode, one file. There's no database server to run alongside
waypointd, no config to point at it, and nothing to back up beyond the
single `.sqlite` file (plus, transiently, its `-wal`/`-shm` sidecars). The
default path is `waypoint.sqlite` in the working directory; `WAYPOINTD_DB_FILE`
overrides it.

## The migration runner

The schema's single source of truth is the SQL files under
`src/database/migrations/`, embedded via `include_str!` and applied in order
by `src/database/migrations.rs`. Every applied file is recorded in the
`schema_migrations` table, so migrations run **once**, forward-only — never
DROP-then-CREATE. Adding a migration is one new `NNNN_name.up.sql` plus one
entry in the `MIGRATIONS` array, nothing else.

There are three migrations today:

- **0001** — the full schema: categories, bookmarks, tags, the
  bookmark_tags junction, the active-only unique indexes on `url` and
  `keyword`, the column-scoped `updated_at` trigger, and the two FTS5
  virtual tables with their trigger set.
- **0002** — ORDER BY-matching partial indexes (`(created_at, id) WHERE
  trashed_at IS NULL`, `(visit_count DESC, id ASC) WHERE trashed_at IS
  NULL`, `(trashed_at, id) WHERE trashed_at IS NOT NULL`) and drops 0001's
  redundant `idx_bookmarks_trashed`. The story there is worth reading if
  you touch list performance: SQLite picks exactly one index per query, and
  the old single-column `trashed_at` index had the same predicate as the
  ordering indexes, so every list request matched on `trashed_at = NULL`
  and then temp-b-tree sorted the whole corpus. Removing it is what lets
  the ordering indexes win.
- **0003** — a NOCASE partial index on `keyword`, so address-bar shortcuts
  stay an index seek regardless of case. The BINARY unique index stays for
  DB-level uniqueness; mixed-case pre-existing rows are tolerated and
  resolved deterministically with `ORDER BY id LIMIT 1`.

`database::open` is the only entry point, and it always leaves the database
at the current version. It also detects and upgrades **legacy** databases —
anything with a `bookmarks` table but no `schema_migrations` row. Those go
through `legacy_preclean` (drop the old unguarded FTS triggers, rename
`deleted_at` → `trashed_at`, drop the dead `mime_type` column), the normal
migration batch, then `legacy_postclean` (scrub trashed rows out of the
main FTS index, and rebuild both indexes if archived rows leaked into the
main one). The migration files are still written with `IF NOT EXISTS`
safety nets because of this — one batch serves fresh and legacy databases
alike.

## Schema

The core table:

```sql
bookmarks (
    id, title, url, description, domain, category_id,
    starred, keyword, note, favicon, thumbnail,
    visit_count, last_visited_at, is_archived, trashed_at,
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

