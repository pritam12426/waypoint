-- waypoint scale indexes (migration 0002)
--
-- 0001 shipped with single-column indexes that cover *point* lookups and the
-- active/trash split, but no index backs the ORDER BYs that drive the web
-- UI list and the dashboard, and the trash view's ORDER BY has nothing to
-- walk. At 1M+ bookmarks every list request was doing a full temp-b-tree
-- sort of the active corpus (see the bench harness in examples/bench.rs).
--
-- The ordering indexes below are *partial on the query's own WHERE
-- predicate* and match their ORDER BY exactly, so a single index serves
-- filter + ordering — no temp-b-tree sort:
--
--   * idx_bookmarks_created (created_at, id) WHERE trashed_at IS NULL —
--     bookmarks::list ORDER BY created_at DESC (backward scan),
--     stats::recently_added, stats::never_visited, and cursor (keyset)
--     pagination WHERE (created_at, id) < (?, ?).
--   * idx_bookmarks_visit (visit_count DESC, id ASC) WHERE trashed_at IS NULL —
--     stats::most_visited ORDER BY visit_count DESC, id ASC (forward scan
--     matches exactly, ties resolved by the index itself).
--   * idx_bookmarks_trash (trashed_at, id) WHERE trashed_at IS NOT NULL —
--     the recycle-bin view ORDER BY trashed_at DESC.
--
-- 0001's `idx_bookmarks_trashed (trashed_at) WHERE trashed_at IS NULL` is
-- DROPPED here. SQLite chooses ONE index per table for a query, and that
-- single-column active-row index had the same predicate as
-- `idx_bookmarks_created` — so every list request matched the index on
-- `trashed_at = NULL` and then temp-b-tree sorted 1M rows. With the
-- redundant index gone, the created/visit/trash partial indexes are the
-- only candidates for their queries, and their ORDER BY-matching order is
-- what the planner picks. The order-less queries (COUNT, aggregates) reuse
-- any of the `trashed_at IS NULL` partial indexes as a counting source; they
-- get their own caching story in later phases.
--
-- Like 0001 this is written to be safe to re-run (IF NOT EXISTS) — legacy
-- databases run through the same batch.

DROP INDEX IF EXISTS idx_bookmarks_trashed;

CREATE INDEX IF NOT EXISTS idx_bookmarks_created
    ON bookmarks(created_at, id)
    WHERE trashed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_visit
    ON bookmarks(visit_count DESC, id ASC)
    WHERE trashed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_trash
    ON bookmarks(trashed_at, id)
    WHERE trashed_at IS NOT NULL;
