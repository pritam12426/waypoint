import { Outlet, createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/bookmarks/$id")({
	component: BookmarkLayout,
});

function BookmarkLayout() {
	return <Outlet />;
}
