# waypointd frontend

A keyboard-first React SPA for the waypointd self-hosted bookmark manager,
talking to the existing Rust/axum backend over its JSON API.

## Stack

React 19 · TanStack Router (file-based) · TanStack Query 5 · TanStack Table/Virtual
· Zustand · Tailwind v4 (CSS-variable theme, dark-first) · Radix primitives styled
shadcn-style · React Hook Form + Zod · cmdk · sonner · Biome.

## Getting started

```bash
npm install
npm run dev      # starts Vite on :3000, proxies /api, /keywords, /open to :8080
```

By default the dev proxy targets `http://localhost:8080`. If your waypointd
server runs on a different port, set `WAYPOINTD_SERVE_PORT` (see
`.env.example`) before starting the dev server.

```bash
npm run build     # tsc -b && vite build -> dist/
npm run preview   # serve the production build locally
npm run check     # biome check --write (lint + format)
```

In production, the waypointd binary serves the contents of `dist/` directly —
no proxy is involved, and the SPA calls the API same-origin.

## Project layout

```
src/
  lib/api/          the stable core — see "API layer" below
    types.ts        TS mirror of the Rust model structs (snake_case, verbatim)
    client.ts        fetch wrapper: auth header, ApiError, x-total-count/
                      x-next-cursor header parsing
    endpoints.ts     one function per backend route, grouped by resource
    query.ts         QueryClient defaults + the qk query-key factory
    hooks.ts         useQuery/useMutation hooks built on the above
  lib/
    state.ts         Zustand: bearer token + theme, persisted to localStorage
    list-nav.ts      Zustand: vim-nav store (active list, active id, actions)
    format.ts        UTC timestamp parsing/formatting (date-fns)
    utils.ts         cn() class merge helper
  hooks/
    use-debounced-value.ts
    use-list-navigation.ts   registers a route's list with the vim-nav store
  components/
    ui/              shadcn-style primitives over Radix (button, dialog, ...)
    app-shell.tsx     sidebar + header + the single global keydown listener
    command-palette.tsx   cmdk: static nav actions + live bookmark search
    keyboard-help.tsx     "?" shortcut reference dialog
    bookmark-form.tsx     RHF + Zod create/edit form, toNew/toUpdateBookmark
    bookmark-media.tsx    Favicon/Thumbnail, sentinel-aware
    tags-input.tsx        chip input for tags
    confirm-dialog.tsx    AlertDialog wrapper for destructive actions
    empty-state.tsx, error-fallback.tsx, kbd.tsx, link.tsx, theme-toggle.tsx
  routes/            one file per page (TanStack Router file-based routing)
```

## API layer

`lib/api/` is the stable core the rest of the app is built on. If you touch
the backend contract, this is the only place that should need to change:

- **`types.ts`** mirrors the Rust structs field-for-field. Keep it
  `snake_case` — this is the wire format, not a place to be idiomatic TS.
- **`client.ts`** is the single `fetch` chokepoint: attaches the bearer token
  from `useApp`, throws a typed `ApiError` (with `.status` and `.code`) on
  non-2xx, and surfaces the lowercase `x-total-count` / `x-next-cursor`
  response headers the list/search endpoints use for pagination.
- **`endpoints.ts`** has one function per backend route. `keywordsApi.list`
  is the one exception to the JSON rule — `GET /keywords` returns
  newline-separated plain text, not JSON.
- **`query.ts`** exports the shared `QueryClient` and the `qk` query-key
  factory. Every query key on every screen goes through `qk.*` so
  `invalidateAll()` (called after any mutation) can't miss a stale cache.
- **`hooks.ts`** is the only place screens should import from — every
  mutation here already wires up cache invalidation and toast feedback.

## Keyboard-first navigation

`components/app-shell.tsx` owns a single `window` keydown listener. Any list
page registers itself with the store in `lib/list-nav.ts` via the
`useListNavigation(ids, actions)` hook (see `routes/bookmarks/index.tsx` for
the fullest example). That's what makes `j`/`k`/`gg`/`G`/`o`/`Enter`/`Y`/`x`/
`s`/`a`/`e`/`d` work identically on any screen with a list, without each
screen re-implementing key handling. `⌘K`/`t` opens the command palette, `/`
focuses search, `?` opens the shortcut reference — all ignored while an
input/textarea/select/contenteditable has focus.

## Media sentinel

`MEDIA_SENTINEL` (`"__default__"`) in `lib/api/types.ts` is a reserved value
for `favicon`/`thumbnail`: it means "use the bundled default asset," as
opposed to `null`/absent (fall back to a letter avatar) or a real URL
(render it). `components/bookmark-media.tsx` and the media-mode radio group
in `bookmark-form.tsx` are the two places that need to know about it.

## Conventions

- Tabs, double quotes, Biome for lint + format (`npm run check`).
- Route params/search params are the source of truth for filter state
  (category/tag/keyword/starred/archived on `/bookmarks`, `q` on `/search`)
  — not component state — so filtered views are shareable/bookmarkable URLs.
- Timestamps from the API are fixed-width UTC strings with no zone suffix;
  always go through `lib/format.ts` (`toDate`/`formatDateTime`/
  `formatRelative`) rather than `new Date(wire)` directly.
- `UpdateBookmark` fields are tri-state: `undefined` leaves a field
  unchanged, `""` clears it. The edit form always sends every editable
  field for exactly this reason — see `toUpdateBookmark` in
  `bookmark-form.tsx`.
