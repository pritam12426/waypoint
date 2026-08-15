import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
	type BookmarkListParams,
	type BulkDeleteParams,
	adminApi,
	authApi,
	bookmarksApi,
	categoriesApi,
	checkApi,
	keywordsApi,
	searchApi,
	statsApi,
	tagsApi,
} from "./endpoints";
import { invalidateAll, qk, queryClient } from "./query";
import type { NewBookmark, UpdateBookmark } from "./types";

// ---------------------------------------------------------------- bookmarks

export function useBookmarks(params: BookmarkListParams = {}) {
	return useQuery({
		queryKey: qk.bookmarkList(params),
		queryFn: async () => {
			const res = await bookmarksApi.list(params);
			return { items: res.data, total: res.headers["x-total-count"] ?? res.data.length };
		},
	});
}

export function useBookmark(id: number) {
	return useQuery({
		queryKey: qk.bookmark(id),
		queryFn: () => bookmarksApi.get(id).then((r) => r.data),
		enabled: Number.isFinite(id),
	});
}

export function useBookmarkNote(id: number) {
	return useQuery({
		queryKey: qk.bookmarkNote(id),
		queryFn: () => bookmarksApi.note(id).then((r) => r.data.note),
		enabled: Number.isFinite(id),
	});
}

export function useCreateBookmark() {
	return useMutation({
		mutationFn: (body: NewBookmark) => bookmarksApi.create(body).then((r) => r.data),
		onSuccess: () => {
			invalidateAll();
			toast.success("Bookmark created");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useUpdateBookmark() {
	return useMutation({
		mutationFn: ({ id, body }: { id: number; body: UpdateBookmark }) =>
			bookmarksApi.update(id, body).then((r) => r.data),
		onSuccess: (data) => {
			queryClient.setQueryData(qk.bookmark(data.id), data);
			invalidateAll();
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useDeleteBookmark() {
	return useMutation({
		mutationFn: ({ id, purge }: { id: number; purge?: boolean }) =>
			bookmarksApi.delete(id, purge),
		onSuccess: (_data, vars) => {
			invalidateAll();
			toast.success(vars.purge ? "Deleted forever" : "Moved to trash");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useRestoreBookmark() {
	return useMutation({
		mutationFn: (id: number) => bookmarksApi.restore(id).then((r) => r.data),
		onSuccess: () => {
			invalidateAll();
			toast.success("Restored");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useCheckBookmark() {
	return useMutation({
		mutationFn: (id: number) => bookmarksApi.check(id).then((r) => r.data),
		onSuccess: (data) => {
			queryClient.setQueryData(qk.bookmark(data.id), data);
			toast.success("Media refreshed");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useBulkDeleteBookmarks() {
	return useMutation({
		mutationFn: (params: BulkDeleteParams) =>
			bookmarksApi.bulkDelete(params).then((r) => r.data),
		onSuccess: (data, vars) => {
			invalidateAll();
			toast.success(
				vars.purge ? `Deleted ${data.removed} forever` : `Trashed ${data.removed}`,
			);
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useBulkUpdateBookmarks() {
	return useMutation({
		mutationFn: ({ ids, update }: { ids: number[]; update: UpdateBookmark }) =>
			bookmarksApi.bulkUpdate(ids, update).then((r) => r.data),
		onSuccess: (data) => {
			invalidateAll();
			toast.success(
				`Updated ${data.updated}${data.skipped ? `, skipped ${data.skipped}` : ""}`,
			);
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useEmptyTrash() {
	return useMutation({
		mutationFn: () => bookmarksApi.emptyTrash(),
		onSuccess: () => {
			invalidateAll();
			toast.success("Trash emptied");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

// --------------------------------------------------------------- categories

export function useCategories() {
	return useQuery({
		queryKey: qk.categories(),
		queryFn: () => categoriesApi.list().then((r) => r.data),
	});
}

export function useRenameCategory() {
	return useMutation({
		mutationFn: ({ id, name }: { id: number; name: string }) =>
			categoriesApi.rename(id, name),
		onSuccess: () => {
			invalidateAll();
			toast.success("Category renamed");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useDeleteCategory() {
	return useMutation({
		mutationFn: (id: number) => categoriesApi.delete(id),
		onSuccess: () => {
			invalidateAll();
			toast.success("Category deleted");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

// -------------------------------------------------------------------- tags

export function useTags() {
	return useQuery({
		queryKey: qk.tags(),
		queryFn: () => tagsApi.list().then((r) => r.data),
	});
}

export function useRenameTag() {
	return useMutation({
		mutationFn: ({ name, newName }: { name: string; newName: string }) =>
			tagsApi.rename(name, newName),
		onSuccess: () => {
			invalidateAll();
			toast.success("Tag renamed");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useDeleteTag() {
	return useMutation({
		mutationFn: (name: string) => tagsApi.delete(name),
		onSuccess: () => {
			invalidateAll();
			toast.success("Tag deleted");
		},
		onError: (err: Error) => toast.error(err.message),
	});
}

// ------------------------------------------------------------------ search

export function useSearch(
	q: string,
	params: { archived?: boolean; limit?: number; offset?: number } = {},
) {
	return useQuery({
		queryKey: qk.search(q, params),
		queryFn: async () => {
			const res = await searchApi.search(q, params);
			return { items: res.data, total: res.headers["x-total-count"] ?? res.data.length };
		},
		enabled: q.trim().length > 0,
	});
}

// ---------------------------------------------------------------- keywords

export function useKeywords() {
	return useQuery({
		queryKey: qk.keywords(),
		queryFn: () => keywordsApi.list(),
	});
}

// ------------------------------------------------------------------- stats

export function useStatsOverview() {
	return useQuery({
		queryKey: qk.stats.overview(),
		queryFn: () => statsApi.overview().then((r) => r.data),
	});
}

export function useStatsDomains(limit?: number, offset?: number) {
	return useQuery({
		queryKey: qk.stats.domains({ limit, offset }),
		queryFn: async () => {
			const res = await statsApi.domains(limit, offset);
			return { items: res.data, total: res.headers["x-total-count"] ?? res.data.length };
		},
	});
}

export function useStatsTags(limit?: number, offset?: number) {
	return useQuery({
		queryKey: qk.stats.tags({ limit, offset }),
		queryFn: () => statsApi.tags(limit, offset).then((r) => r.data),
	});
}

export function useStatsTopVisited() {
	return useQuery({
		queryKey: qk.stats.topVisited(),
		queryFn: () => statsApi.topVisited().then((r) => r.data),
	});
}

export function useStatsNeverVisited(limit?: number, offset?: number) {
	return useQuery({
		queryKey: qk.stats.neverVisited({ limit, offset }),
		queryFn: async () => {
			const res = await statsApi.neverVisited(limit, offset);
			return { items: res.data, total: res.headers["x-total-count"] ?? res.data.length };
		},
	});
}

export function useStatsOrphanTags(limit?: number, offset?: number) {
	return useQuery({
		queryKey: qk.stats.orphanTags({ limit, offset }),
		queryFn: () => statsApi.orphanTags(limit, offset).then((r) => r.data),
	});
}

export function useStatsHygiene() {
	return useQuery({
		queryKey: qk.stats.hygiene(),
		queryFn: () => statsApi.hygiene().then((r) => r.data),
	});
}

export function useStatsActivity() {
	return useQuery({
		queryKey: qk.stats.activity(),
		queryFn: () => statsApi.activity().then((r) => r.data),
	});
}

// ------------------------------------------------------------------- admin

export function useBackup() {
	return useMutation({
		mutationFn: () => adminApi.backup(),
		onSuccess: () => toast.success("Backup started"),
		onError: (err: Error) => toast.error(err.message),
	});
}

export function useCheckJob() {
	return useMutation({
		mutationFn: () => checkApi.start().then((r) => r.data),
		onError: (err: Error) => toast.error(err.message),
	});
}

// -------------------------------------------------------------------- auth

export function useAuthStatus() {
	return useQuery({
		queryKey: qk.auth.status(),
		queryFn: () => authApi.status().then((r) => r.data),
		retry: false,
	});
}

// avoid unused-import lint on the shared queryClient re-export point
export { useQueryClient };
