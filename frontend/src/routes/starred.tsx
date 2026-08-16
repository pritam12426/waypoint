import { createFileRoute } from "@tanstack/react-router";
import { BookmarksList, bookmarkListSearchSchema } from "#/components/bookmarks-list";

export const Route = createFileRoute("/starred")({
	validateSearch: bookmarkListSearchSchema,
	component: StarredPage,
});

function StarredPage() {
	const search = Route.useSearch();
	return <BookmarksList search={{ ...search, starred: true }} to="/starred" hideStar showTotal />;
}
