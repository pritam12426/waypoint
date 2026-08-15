import { createFileRoute } from "@tanstack/react-router";
import {
	ArchiveRestore,
	ExternalLink,
	Pencil,
	RotateCcw,
	Star,
	Trash2,
} from "lucide-react";
import { useState } from "react";
import { Thumbnail } from "#/components/bookmark-media";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Link } from "#/components/link";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import {
	useBookmark,
	useBookmarkNote,
	useDeleteBookmark,
	useRestoreBookmark,
	useUpdateBookmark,
} from "#/lib/api/hooks";
import { formatDateTime, formatRelative } from "#/lib/format";

export const Route = createFileRoute("/bookmarks/$id/")({
	component: BookmarkDetailPage,
});

function BookmarkDetailPage() {
	const { id } = Route.useParams();
	const navigate = Route.useNavigate();
	const bookmarkId = Number(id);
	const { data: bm, isLoading } = useBookmark(bookmarkId);
	const { data: note } = useBookmarkNote(bookmarkId);
	const updateBookmark = useUpdateBookmark();
	const deleteBookmark = useDeleteBookmark();
	const restoreBookmark = useRestoreBookmark();
	const [confirmTrash, setConfirmTrash] = useState(false);
	const [confirmPurge, setConfirmPurge] = useState(false);

	if (isLoading || !bm) {
		return (
			<div className="mx-auto max-w-2xl space-y-4">
				<Skeleton className="h-8 w-2/3" />
				<Skeleton className="h-40 w-full" />
			</div>
		);
	}

	return (
		<div className="mx-auto max-w-2xl space-y-6">
			{bm.trashed_at && (
				<div className="flex items-center justify-between rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm">
					<span>Trashed {formatRelative(bm.trashed_at)}</span>
					<div className="flex gap-2">
						<Button
							size="sm"
							variant="outline"
							onClick={() => restoreBookmark.mutate(bm.id)}
						>
							<RotateCcw /> Restore
						</Button>
						<Button size="sm" variant="destructive" onClick={() => setConfirmPurge(true)}>
							Delete forever
						</Button>
					</div>
				</div>
			)}

			<div className="flex items-start justify-between gap-4">
				<h1 className="text-2xl font-semibold">{bm.title || bm.url}</h1>
				<div className="flex shrink-0 gap-1">
					<Button
						variant="ghost"
						size="icon"
						onClick={() =>
							updateBookmark.mutate({ id: bm.id, body: { starred: !bm.starred } })
						}
						aria-label="Star"
					>
						<Star className={bm.starred ? "fill-primary text-primary" : ""} />
					</Button>
					<Button
						variant="ghost"
						size="icon"
						onClick={() =>
							updateBookmark.mutate({ id: bm.id, body: { is_archived: !bm.is_archived } })
						}
						aria-label="Archive"
					>
						<ArchiveRestore className={bm.is_archived ? "text-primary" : ""} />
					</Button>
					<Button variant="ghost" size="icon" asChild aria-label="Edit">
						<Link to="/bookmarks/$id/edit" params={{ id: String(bm.id) }}>
							<Pencil />
						</Link>
					</Button>
					{!bm.trashed_at && (
						<Button
							variant="ghost"
							size="icon"
							onClick={() => setConfirmTrash(true)}
							aria-label="Trash"
						>
							<Trash2 />
						</Button>
					)}
				</div>
			</div>

			<a
				href={`/open/${bm.id}`}
				target="_blank"
				rel="noreferrer"
				className="flex items-center gap-1.5 text-sm text-primary hover:underline"
			>
				{bm.url} <ExternalLink className="size-3.5" />
			</a>

			{bm.description && (
				<p className="text-sm text-muted-foreground">{bm.description}</p>
			)}

			{bm.thumbnail && (
				<Thumbnail src={bm.thumbnail} domain={bm.domain} className="max-w-md" />
			)}

			{note && (
				<Card>
					<CardContent className="whitespace-pre-wrap p-4 text-sm">{note}</CardContent>
				</Card>
			)}

			<Card>
				<CardContent className="grid grid-cols-2 gap-3 p-4 text-sm">
					<div>
						<p className="text-muted-foreground">Category</p>
						<p>{bm.category}</p>
					</div>
					<div>
						<p className="text-muted-foreground">Keyword</p>
						<p>{bm.keyword || "—"}</p>
					</div>
					<div>
						<p className="text-muted-foreground">Visits</p>
						<p>{bm.visit_count}</p>
					</div>
					<div>
						<p className="text-muted-foreground">Last visited</p>
						<p>{bm.last_visited_at ? formatRelative(bm.last_visited_at) : "Never"}</p>
					</div>
					<div>
						<p className="text-muted-foreground">Added</p>
						<p>{formatDateTime(bm.created_at)}</p>
					</div>
					<div>
						<p className="text-muted-foreground">Updated</p>
						<p>{formatDateTime(bm.updated_at)}</p>
					</div>
					{bm.tags.length > 0 && (
						<div className="col-span-2">
							<p className="mb-1 text-muted-foreground">Tags</p>
							<div className="flex flex-wrap gap-1">
								{bm.tags.map((t) => (
									<Badge key={t} variant="secondary">
										{t}
									</Badge>
								))}
							</div>
						</div>
					)}
				</CardContent>
			</Card>

			<ConfirmDialog
				open={confirmTrash}
				onOpenChange={setConfirmTrash}
				title="Trash this bookmark?"
				description="You can restore it from the trash later."
				confirmLabel="Trash"
				onConfirm={async () => {
					await deleteBookmark.mutateAsync({ id: bm.id });
					navigate({ to: "/bookmarks" });
				}}
			/>
			<ConfirmDialog
				open={confirmPurge}
				onOpenChange={setConfirmPurge}
				title="Delete forever?"
				description="This cannot be undone."
				confirmLabel="Delete forever"
				onConfirm={async () => {
					await deleteBookmark.mutateAsync({ id: bm.id, purge: true });
					navigate({ to: "/trash" });
				}}
			/>
		</div>
	);
}
