-- waypoint initial schema (migration 0001)
--
-- Unlike the old single `migrations/001_initial.sql` (which re-ran on every
-- startup), this file runs exactly once per database, tracked by the
-- `schema_migrations` table. It is *still* written to be safe to re-run,
-- though: legacy databases upgraded by `database::open()` go through this
-- same batch, so `CREATE ... IF NOT EXISTS` and the `DROP ... IF EXISTS`
-- safety nets stay — the cost is nil and it keeps one batch safe for both
-- the fresh and the legacy path.
--
-- `PRAGMA foreign_keys` is intentionally absent: it must be set on the
-- connection *before* any transaction begins (it's a no-op inside one), and
-- `database::open()` already does that.

-- ============================================================
-- 1. CATEGORIES
-- ============================================================
CREATE TABLE IF NOT EXISTS categories (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT    UNIQUE NOT NULL
);

-- ============================================================
-- 2. BOOKMARKS (core table)
--
-- Recycle bin via `trashed_at`: NULL = active, non-NULL = in the trash.
-- `url` and `keyword` are unique only among *active* rows (see the partial
-- unique indexes below), so a trashed bookmark never blocks re-adding the
-- same URL or keyword.
-- ============================================================
CREATE TABLE IF NOT EXISTS bookmarks (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT NOT NULL,
    url             TEXT NOT NULL,
    description     TEXT,
    domain          TEXT,
    category_id     INTEGER NOT NULL,
    starred         BOOLEAN DEFAULT 0,
    keyword         TEXT,
    note            TEXT,
    favicon         TEXT,
    thumbnail       TEXT,
    visit_count     INTEGER DEFAULT 0,
    last_visited_at DATETIME,
    is_archived     BOOLEAN DEFAULT 0,
    trashed_at      DATETIME DEFAULT NULL,
    created_at      DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at      DATETIME DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (category_id) REFERENCES categories(id) ON DELETE CASCADE
);

-- ============================================================
-- 3. TAGS
-- ============================================================
CREATE TABLE IF NOT EXISTS tags (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL
);

-- ============================================================
-- 4. JUNCTION TABLE (bookmarks <-> tags)
-- ============================================================
CREATE TABLE IF NOT EXISTS bookmark_tags (
    bookmark_id INTEGER NOT NULL,
    tag_id      INTEGER NOT NULL,
    PRIMARY KEY (bookmark_id, tag_id),
    FOREIGN KEY (bookmark_id) REFERENCES bookmarks(id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id)      REFERENCES tags(id) ON DELETE CASCADE
);

-- ============================================================
-- 5. INDEXES
-- ============================================================
CREATE UNIQUE INDEX IF NOT EXISTS idx_bookmarks_url_active
    ON bookmarks(url) WHERE trashed_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_bookmarks_keyword_active
    ON bookmarks(keyword) WHERE trashed_at IS NULL;

DROP INDEX IF EXISTS idx_bookmarks_deleted;
CREATE INDEX IF NOT EXISTS idx_bookmarks_trashed ON bookmarks(trashed_at) WHERE trashed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_bookmarks_category ON bookmarks(category_id);
CREATE INDEX IF NOT EXISTS idx_bookmarks_domain   ON bookmarks(domain);
CREATE INDEX IF NOT EXISTS idx_bookmarks_starred  ON bookmarks(starred);
CREATE INDEX IF NOT EXISTS idx_bookmarks_archived ON bookmarks(is_archived);
CREATE INDEX IF NOT EXISTS idx_tags_name          ON tags(name);
CREATE INDEX IF NOT EXISTS idx_bookmark_tags_tag  ON bookmark_tags(tag_id);

-- ============================================================
-- 6. TRIGGER: auto-update `updated_at` on real edits only
--
-- Scoped to the columns a user edit actually touches (via `OF ...`), so
-- that visit-tracking writes (`visit_count`, `last_visited_at`, done by
-- record_visit()) don't bump `updated_at` — "last modified" and "last
-- visited" are different signals and shouldn't collapse into one.
--
-- The WHEN guard is a defensive no-recursion measure: this trigger's own
-- UPDATE wouldn't normally re-fire itself (SQLite's `recursive_triggers`
-- pragma defaults to OFF), but the guard keeps that true even if something
-- else in the process turns that pragma on later.
-- ============================================================
CREATE TRIGGER IF NOT EXISTS update_bookmark_timestamp
AFTER UPDATE OF
    title, url, description, domain, category_id, starred, keyword,
    note, favicon, thumbnail, is_archived, trashed_at
ON bookmarks
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE bookmarks SET updated_at = CURRENT_TIMESTAMP WHERE id = NEW.id;
END;

-- ============================================================
-- 7. FULL-TEXT SEARCH (FTS5, external-content so data isn't duplicated)
--
