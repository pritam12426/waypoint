import { QueryClient } from "@tanstack/react-query";
import { ApiError } from "./client";

export const queryClient = new QueryClient({
	defaultOptions: {
		queries: {
			staleTime: 30_000,
			gcTime: 5 * 60_000,
			retry: (failureCount, error) => {
				if (error instanceof ApiError && error.status < 500) return false;
				return failureCount < 2;
			},
			refetchOnWindowFocus: false,
		},
		mutations: {
			retry: false,
		},
	},
});

/**
 * Query key factory. Keep this shape stable — every screen relies on
 * `qk.*` + `invalidateAll()` to stay consistent without manual cache
 * bookkeeping.
 */
export const qk = {
	all: ["waypointd"] as const,
	bookmarks: () => [...qk.all, "bookmarks"] as const,
	bookmarkList: (params: unknown) => [...qk.bookmarks(), "list", params] as const,
	bookmark: (id: number) => [...qk.bookmarks(), "detail", id] as const,
	bookmarkNote: (id: number) => [...qk.bookmarks(), "note", id] as const,
	categories: () => [...qk.all, "categories"] as const,
	tags: () => [...qk.all, "tags"] as const,
	keywords: () => [...qk.all, "keywords"] as const,
	search: (q: string, params: unknown) => [...qk.all, "search", q, params] as const,
	stats: {
		overview: () => [...qk.all, "stats", "overview"] as const,
		domains: (page: unknown) => [...qk.all, "stats", "domains", page] as const,
		tags: (page: unknown) => [...qk.all, "stats", "tags", page] as const,
		topVisited: () => [...qk.all, "stats", "top-visited"] as const,
		neverVisited: (page: unknown) => [...qk.all, "stats", "never-visited", page] as const,
		orphanTags: (page: unknown) => [...qk.all, "stats", "orphan-tags", page] as const,
		hygiene: () => [...qk.all, "stats", "hygiene"] as const,
		activity: () => [...qk.all, "stats", "activity"] as const,
	},
	auth: {
		status: () => [...qk.all, "auth", "status"] as const,
	},
};

/** Invalidate every cached query. Call after any mutation whose blast
 * radius is unclear (bulk ops, imports, category/tag renames). */
export function invalidateAll() {
	return queryClient.invalidateQueries({ queryKey: qk.all });
}
