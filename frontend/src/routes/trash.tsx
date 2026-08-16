import { createFileRoute } from "@tanstack/react-router";
import { RotateCcw, Trash2 } from "lucide-react";
import { useState } from "react";
import { Favicon } from "#/components/bookmark-media";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { EmptyState } from "#/components/empty-state";
import { Link } from "#/components/link";
import { Button } from "#/components/ui/button";
import { Skeleton } from "#/components/ui/skeleton";
import {
	useBookmarks,
	useDeleteBookmark,
	useEmptyTrash,
	useRestoreBookmark,
} from "#/lib/api/hooks";
import { formatRelative } from "#/lib/format";

export const Route = createFileRoute("/trash")({
	component: TrashPage,
});

function TrashPage() {
	const { data, isLoading } = useBookmarks({ trash: true, limit: 50 });
	const restoreBookmark = useRestoreBookmark();
	const deleteBookmark = useDeleteBookmark();
	const emptyTrash = useEmptyTrash();
	const [confirmEmpty, setConfirmEmpty] = useState(false);

	const items = data?.items ?? [];

	return (
		<div className="space-y-4">
			<div className="flex items-center justify-end">
				{items.length > 0 && (
					<Button variant="destructive" size="sm" onClick={() => setConfirmEmpty(true)}>
						Empty trash
					</Button>
				)}
			</div>

			{isLoading && (
				<div className="space-y-1">
					{Array.from({ length: 6 }).map((_, i) => (
						<Skeleton key={`t-${i}`} className="h-11 w-full" />
					))}
				</div>
			)}

			{!isLoading && items.length === 0 && (
				<EmptyState icon={Trash2} title="Trash is empty" />
			)}

			<div className="space-y-1">
				{items.map((bm) => (
					<div
						key={bm.id}
						className="flex items-center gap-3 rounded-md border border-border px-3 py-2 text-sm"
					>
						<Favicon src={bm.favicon} domain={bm.domain} />
						<Link
							to="/bookmarks/$id"
							params={{ id: String(bm.id) }}
							className="min-w-0 flex-1 truncate hover:underline"
						>
							{bm.title || bm.url}
						</Link>
						<span className="shrink-0 text-xs text-muted-foreground">
							Trashed {bm.trashed_at ? formatRelative(bm.trashed_at) : ""}
						</span>
						<Button
							size="sm"
							variant="outline"
							onClick={() => restoreBookmark.mutate(bm.id)}
						>
							<RotateCcw /> Restore
						</Button>
						<Button
							size="sm"
							variant="destructive"
							onClick={() => deleteBookmark.mutate({ id: bm.id, purge: true })}
						>
							Delete forever
						</Button>
					</div>
				))}
			</div>

			<ConfirmDialog
				open={confirmEmpty}
				onOpenChange={setConfirmEmpty}
				title="Empty trash?"
				description="All trashed bookmarks will be permanently deleted. This cannot be undone."
				confirmLabel="Empty trash"
				onConfirm={async () => {
					await emptyTrash.mutateAsync();
				}}
			/>
		</div>
	);
}
