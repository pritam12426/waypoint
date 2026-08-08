-- Keywords are URL path segments at /keywords/{keyword}, typed into a
-- browser address bar where case is informal (`II`, `ii`, `Ii` should all
-- resolve to the same shortcut). This migration adds a case-insensitive
-- (NOCASE) partial index so those lookups are index seeks instead of full
-- scans.
--
-- The existing `idx_bookmarks_keyword_active` (BINARY collation) stays: it
-- still enforces exact-case uniqueness at the DB level, and the friendly
-- duplicate pre-checks in `database::bookmarks` are now NOCASE too, so no
-- new mixed-case pair can be created. Pre-existing mixed-case rows (which
-- the BINARY index allowed) are tolerated: `get_by_keyword` pins
-- `ORDER BY id LIMIT 1` for a deterministic pick.
CREATE INDEX IF NOT EXISTS idx_bookmarks_keyword_nocase
    ON bookmarks(keyword COLLATE NOCASE) WHERE trashed_at IS NULL;
