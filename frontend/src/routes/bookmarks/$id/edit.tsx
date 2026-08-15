import { createFileRoute } from "@tanstack/react-router";
import {
	BookmarkForm,
	type BookmarkFormValues,
	toUpdateBookmark,
} from "#/components/bookmark-form";
import { Skeleton } from "#/components/ui/skeleton";
import { useBookmark, useCategories, useUpdateBookmark } from "#/lib/api/hooks";
import { MEDIA_SENTINEL } from "#/lib/api/types";

export const Route = createFileRoute("/bookmarks/$id/edit")({
	component: EditBookmarkPage,
});

function EditBookmarkPage() {
	const { id } = Route.useParams();
	const navigate = Route.useNavigate();
	const bookmarkId = Number(id);
	const { data: bm, isLoading } = useBookmark(bookmarkId);
	const { data: categories = [] } = useCategories();
	const updateBookmark = useUpdateBookmark();

	if (isLoading || !bm) {
		return (
			<div className="mx-auto max-w-xl space-y-4">
				<Skeleton className="h-8 w-1/2" />
				<Skeleton className="h-96 w-full" />
			</div>
		);
	}

	const defaultValues: Partial<BookmarkFormValues> = {
		url: bm.url,
		title: bm.title,
		category: bm.category ?? "",
		description: bm.description ?? "",
		tags: bm.tags,
		keyword: bm.keyword ?? "",
		redirectTemplate: bm.redirect_template ?? "",
		note: bm.note ?? "",
		starred: bm.starred,
		faviconMode:
			bm.favicon === MEDIA_SENTINEL ? "default" : bm.favicon ? "fetch" : "auto",
		faviconUrl: bm.favicon && bm.favicon !== MEDIA_SENTINEL ? bm.favicon : "",
		thumbnailMode:
			bm.thumbnail === MEDIA_SENTINEL ? "default" : bm.thumbnail ? "fetch" : "auto",
		thumbnailUrl: bm.thumbnail && bm.thumbnail !== MEDIA_SENTINEL ? bm.thumbnail : "",
	};

	return (
		<div className="mx-auto max-w-xl space-y-4">
			<h1 className="text-xl font-semibold">Edit bookmark</h1>
			<BookmarkForm
				categories={categories}
				defaultValues={defaultValues}
				submitLabel="Save changes"
				pending={updateBookmark.isPending}
				onCancel={() => navigate({ to: "/bookmarks/$id", params: { id } })}
				onSubmit={(values) => {
					updateBookmark.mutate(
						{ id: bookmarkId, body: toUpdateBookmark(values) },
						{ onSuccess: () => navigate({ to: "/bookmarks/$id", params: { id } }) },
					);
				}}
			/>
		</div>
	);
}
