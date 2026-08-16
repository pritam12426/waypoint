import { createFileRoute } from "@tanstack/react-router";
import { BookmarkForm, toNewBookmark } from "#/components/bookmark-form";
import { useCategories, useCreateBookmark } from "#/lib/api/hooks";

export const Route = createFileRoute("/bookmarks/new")({
	component: NewBookmarkPage,
});

function NewBookmarkPage() {
	const navigate = Route.useNavigate();
	const { data: categories = [] } = useCategories();
	const createBookmark = useCreateBookmark();

	return (
		<div className="mx-auto max-w-xl space-y-4">
			<BookmarkForm
				categories={categories}
				pending={createBookmark.isPending}
				onCancel={() => navigate({ to: "/bookmarks" })}
				onSubmit={(values) => {
					createBookmark.mutate(toNewBookmark(values), {
						onSuccess: (bm) =>
							navigate({ to: "/bookmarks/$id", params: { id: String(bm.id) } }),
					});
				}}
			/>
		</div>
	);
}
