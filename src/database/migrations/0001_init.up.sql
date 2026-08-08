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
-- Two mirrored indexes over the same `bookmarks` content table:
--   * bookmarks_fts          — active bookmarks only
--   * bookmarks_fts_archived — archived bookmarks only
-- A bookmark's content lives in exactly one of three places, enforced by the
-- triggers below: active (`trashed_at IS NULL`, `is_archived = 0`) → main,
-- archived (`trashed_at IS NULL`, `is_archived = 1`) → archive, trashed
-- (`trashed_at IS NOT NULL`) → neither. Trash and archive are therefore
-- quarantined at the raw index level: main search physically cannot match
-- archived or trashed content, and archive search only ever sees archived
-- rows.
-- ============================================================
-- Safety net for the legacy `deleted_at` databases: the old trash-trigger
-- name is dropped so upgraded databases end up with only the
-- `bookmarks_fts_trash` name.
DROP TRIGGER IF EXISTS bookmarks_fts_soft_delete;

CREATE VIRTUAL TABLE IF NOT EXISTS bookmarks_fts USING fts5(
    title,
    description,
    note,
    url,
    content=bookmarks,
    content_rowid=id
);

CREATE VIRTUAL TABLE IF NOT EXISTS bookmarks_fts_archived USING fts5(
    title,
    description,
    note,
    url,
    content=bookmarks,
    content_rowid=id
);

-- INSERT: route by is_archived.
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_insert AFTER INSERT ON bookmarks
WHEN NEW.is_archived = 0
BEGIN
    INSERT INTO bookmarks_fts(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archived_insert AFTER INSERT ON bookmarks
WHEN NEW.is_archived = 1
BEGIN
    INSERT INTO bookmarks_fts_archived(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

-- DELETE (hard purge): remove from the index that actually holds the row.
-- Trashed rows hold no index entry, hence the trashed_at guard. The
-- is_archived guard routes by OLD state — the index the content currently
-- lives in.
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_delete AFTER DELETE ON bookmarks
WHEN OLD.trashed_at IS NULL AND OLD.is_archived = 0
BEGIN
    INSERT INTO bookmarks_fts(bookmarks_fts, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archived_delete AFTER DELETE ON bookmarks
WHEN OLD.trashed_at IS NULL AND OLD.is_archived = 1
BEGIN
    INSERT INTO bookmarks_fts_archived(bookmarks_fts_archived, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
END;

-- Content edits (title/description/note/url): re-sync only when the row is
-- active in that index. The OLD == NEW is_archived guards are load-bearing:
-- `update --archive` rewrites every column in one UPDATE, firing these
-- triggers AND the archive-toggle triggers together, so they must not fight
-- over a row that is mid-move — only the toggle triggers act then.
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_update
AFTER UPDATE OF title, description, note, url ON bookmarks
WHEN OLD.trashed_at IS NULL AND NEW.trashed_at IS NULL
    AND OLD.is_archived = 0 AND NEW.is_archived = 0
BEGIN
    INSERT INTO bookmarks_fts(bookmarks_fts, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
    INSERT INTO bookmarks_fts(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archived_update
AFTER UPDATE OF title, description, note, url ON bookmarks
WHEN OLD.trashed_at IS NULL AND NEW.trashed_at IS NULL
    AND OLD.is_archived = 1 AND NEW.is_archived = 1
BEGIN
    INSERT INTO bookmarks_fts_archived(bookmarks_fts_archived, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
    INSERT INTO bookmarks_fts_archived(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

-- Moving to trash removes the row from whichever index holds it, so trashed
-- content is quarantined even at the raw index level. trashed_at is not one
-- of the columns FTS indexes, so it is handled by its own triggers rather
-- than added to the update triggers above. OLD state picks the source index
-- (where the content lives right now).
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_trash
AFTER UPDATE OF trashed_at ON bookmarks
WHEN OLD.trashed_at IS NULL AND NEW.trashed_at IS NOT NULL AND OLD.is_archived = 0
BEGIN
    INSERT INTO bookmarks_fts(bookmarks_fts, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archived_trash
AFTER UPDATE OF trashed_at ON bookmarks
WHEN OLD.trashed_at IS NULL AND NEW.trashed_at IS NOT NULL AND OLD.is_archived = 1
BEGIN
    INSERT INTO bookmarks_fts_archived(bookmarks_fts_archived, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
END;

-- Restoring puts content back into the index matching the post-restore state
-- (NEW.is_archived): an archived bookmark restores into the archive index.
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_restore
AFTER UPDATE OF trashed_at ON bookmarks
WHEN NEW.trashed_at IS NULL AND OLD.trashed_at IS NOT NULL AND NEW.is_archived = 0
BEGIN
    INSERT INTO bookmarks_fts(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archived_restore
AFTER UPDATE OF trashed_at ON bookmarks
WHEN NEW.trashed_at IS NULL AND OLD.trashed_at IS NOT NULL AND NEW.is_archived = 1
BEGIN
    INSERT INTO bookmarks_fts_archived(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

-- Archive toggle moves content between the two indexes in one shot. Only
-- is_archived changed, so OLD and NEW content are identical; the delete uses
-- OLD values and the insert NEW values. Guarded to active rows: a trashed
-- bookmark holds no index entry, so there is nothing to move (the restore
-- trigger re-adds it to the right index by NEW.is_archived).
CREATE TRIGGER IF NOT EXISTS bookmarks_fts_archive
AFTER UPDATE OF is_archived ON bookmarks
WHEN OLD.is_archived = 0 AND NEW.is_archived = 1
    AND OLD.trashed_at IS NULL AND NEW.trashed_at IS NULL
BEGIN
    INSERT INTO bookmarks_fts(bookmarks_fts, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
    INSERT INTO bookmarks_fts_archived(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;

CREATE TRIGGER IF NOT EXISTS bookmarks_fts_unarchive
AFTER UPDATE OF is_archived ON bookmarks
WHEN OLD.is_archived = 1 AND NEW.is_archived = 0
    AND OLD.trashed_at IS NULL AND NEW.trashed_at IS NULL
BEGIN
    INSERT INTO bookmarks_fts_archived(bookmarks_fts_archived, rowid, title, description, note, url)
    VALUES ('delete', OLD.id, OLD.title, OLD.description, OLD.note, OLD.url);
    INSERT INTO bookmarks_fts(rowid, title, description, note, url)
    VALUES (NEW.id, NEW.title, NEW.description, NEW.note, NEW.url);
END;
