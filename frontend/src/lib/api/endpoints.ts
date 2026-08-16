import { apiRequest } from "./client";
import type {
	ActivityGranularity,
	ActivityPoint,
	AuthStatus,
	Bookmark,
	BulkDeleteResult,
	BulkUpdateResult,
	Category,
	CheckJob,
	DomainStat,
	Hygiene,
	InactiveBookmark,
	NeverVisited,
	NewBookmark,
	StatsOverview,
	Tag,
	UpdateBookmark,
} from "./types";

export interface BookmarkListParams {
	category?: string;
	category_id?: number;
	tag?: string;
	keyword?: string;
	starred?: boolean;
	archived?: boolean;
	trash?: boolean;
	created_after?: string;
	created_before?: string;
	updated_after?: string;
	updated_before?: string;
	visited_after?: string;
	visited_before?: string;
	trashed_after?: string;
	trashed_before?: string;
	limit?: number;
	offset?: number;
	cursor?: string;
}

export interface BulkDeleteParams {
	ids?: number[];
	category?: string;
	tag?: string;
	keyword?: string;
	starred?: boolean;
	archived?: boolean;
	trash?: boolean;
	purge?: boolean;
	dry_run?: boolean;
}

export type RandomParams = {
	limit?: number;
	category?: string;
	tag?: string;
	starred?: boolean;
	archived?: boolean;
	/** `all` (default), `never_visited`, or `unseen_90d`. */
	pool?: "all" | "never_visited" | "unseen_90d";
} & Record<string, string | number | boolean | null | undefined>;

export const bookmarksApi = {
	list: (params: BookmarkListParams = {}) =>
		apiRequest<Bookmark[]>("/api/bookmarks", {
			params: { ...params, ids: undefined },
		}),

	get: (id: number) => apiRequest<Bookmark>(`/api/bookmarks/${id}`),

	create: (body: NewBookmark) =>
		apiRequest<Bookmark>("/api/bookmarks", { method: "POST", body }),

	update: (id: number, body: UpdateBookmark) =>
		apiRequest<Bookmark>(`/api/bookmarks/${id}`, { method: "PUT", body }),

	delete: (id: number, purge = false) =>
		apiRequest<void>(`/api/bookmarks/${id}`, {
			method: "DELETE",
			params: { purge },
		}),

	restore: (id: number) =>
		apiRequest<Bookmark>(`/api/bookmarks/${id}/restore`, { method: "POST" }),

	check: (id: number) => apiRequest<Bookmark>(`/api/bookmarks/${id}/check`),

	note: (id: number) => apiRequest<{ note: string }>(`/api/bookmarks/${id}/note`),

	bulkDelete: (params: BulkDeleteParams) =>
		apiRequest<BulkDeleteResult>("/api/bookmarks", {
			method: "DELETE",
			params: {
				...params,
				ids: params.ids?.join(","),
			},
		}),

	bulkUpdate: (ids: number[], update: UpdateBookmark) =>
		apiRequest<BulkUpdateResult>("/api/bookmarks", {
			method: "PATCH",
			body: { ids, update },
		}),

	emptyTrash: () => apiRequest<void>("/api/trash", { method: "DELETE" }),
	random: (params: RandomParams = {}) =>
		apiRequest<Bookmark[]>("/api/bookmarks/random", { params }),
};

export const categoriesApi = {
	list: () => apiRequest<Category[]>("/api/categories"),
	rename: (id: number, name: string) =>
		apiRequest<Category>(`/api/categories/${id}`, { method: "PUT", body: { name } }),
	delete: (id: number) => apiRequest<void>(`/api/categories/${id}`, { method: "DELETE" }),
};

export const tagsApi = {
	list: () => apiRequest<Tag[]>("/api/tags"),
	rename: (name: string, newName: string) =>
		apiRequest<Tag>(`/api/tags/${encodeURIComponent(name)}`, {
			method: "PUT",
			body: { name: newName.toLowerCase() },
		}),
	delete: (name: string) =>
		apiRequest<void>(`/api/tags/${encodeURIComponent(name)}`, { method: "DELETE" }),
};

export const searchApi = {
	search: (
		q: string,
		params: { archived?: boolean; limit?: number; offset?: number } = {},
	) => apiRequest<Bookmark[]>("/api/search", { params: { q, ...params } }),
};

export const keywordsApi = {
	/** GET /keywords is plain text (one per line), not JSON — fetch directly. */
	list: async (): Promise<string[]> => {
		const res = await fetch("/keywords");
		const text = await res.text();
		return text.split("\n").filter(Boolean);
	},
};

export const statsApi = {
	overview: () => apiRequest<StatsOverview>("/api/stats"),
	domains: (limit?: number, offset?: number) =>
		apiRequest<DomainStat[]>("/api/stats/domains", { params: { limit, offset } }),
	tags: (limit?: number, offset?: number) =>
		apiRequest<Tag[]>("/api/stats/tags", { params: { limit, offset } }),
	topVisited: () => apiRequest<Bookmark[]>("/api/stats/top-visited"),
	neverVisited: (limit?: number, offset?: number) =>
		apiRequest<NeverVisited[]>("/api/stats/never-visited", { params: { limit, offset } }),
	inactive: (limit?: number, offset?: number) =>
		apiRequest<InactiveBookmark[]>("/api/stats/inactive", { params: { limit, offset } }),
	hygiene: () => apiRequest<Hygiene>("/api/stats/hygiene"),
	activity: (granularity: ActivityGranularity, limit?: number) =>
		apiRequest<ActivityPoint[]>("/api/stats/activity", { params: { granularity, limit } }),
};

export const adminApi = {
	backup: () => apiRequest<void>("/api/admin/backup", { method: "POST" }),
};

export const authApi = {
	status: () => apiRequest<AuthStatus>("/api/auth/status"),
};

export const checkApi = {
	start: () => apiRequest<CheckJob>("/api/check", { method: "POST" }),
	status: (id: string) => apiRequest<CheckJob>(`/api/check/${id}`),
};
