import { createFileRoute } from "@tanstack/react-router";
import { BookmarksList, bookmarkListSearchSchema } from "#/components/bookmarks-list";

export const Route = createFileRoute("/archived")({
	validateSearch: bookmarkListSearchSchema,
	component: ArchivedPage,
});

function ArchivedPage() {
	const search = Route.useSearch();
	return (
		<BookmarksList search={{ ...search, archived: true }} to="/archived" showTotal />
	);
}
