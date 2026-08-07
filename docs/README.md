# Documentation

These pages cover how waypointd works, how to run it, and how to change it.
There's no prescribed reading order — each page stands on its own and
cross-references the others when it matters.

- [architecture.md](architecture.md) — the crate layout, why the modules
  are split the way they are, and the threading model behind the HTTP layer.
- [api.md](api.md) — the full HTTP surface: auth, pagination, error codes,
  and a walkthrough of every endpoint.
- [database.md](database.md) — the SQLite schema, the migration runner,
  full-text search, and the WAL setup.
- [operations.md](operations.md) — environment variables, logging, the
  media cache, backups, and other day-to-day concerns.
- [contributing.md](contributing.md) — building, testing, and the
  conventions to follow before you touch code.

waypointd was written from scratch. There is only a server — no CLI, no
legacy, nothing carried over from an earlier design — so the codebase
starts from what a personal bookmark server actually needs.
