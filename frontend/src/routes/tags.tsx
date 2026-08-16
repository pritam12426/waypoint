import { createFileRoute } from "@tanstack/react-router";
import { Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { EmptyState } from "#/components/empty-state";
import { Link } from "#/components/link";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Skeleton } from "#/components/ui/skeleton";
import { useDeleteTag, useRenameTag, useTags } from "#/lib/api/hooks";

export const Route = createFileRoute("/tags")({
	component: TagsPage,
});

function TagsPage() {
	const { data: tags = [], isLoading } = useTags();
	const renameTag = useRenameTag();
	const deleteTag = useDeleteTag();
	const [editing, setEditing] = useState<string | null>(null);
	const [draft, setDraft] = useState("");
	const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

	return (
		<div className="mx-auto max-w-xl space-y-4">
			{isLoading && (
				<div className="space-y-1">
					{Array.from({ length: 6 }).map((_, i) => (
						<Skeleton key={`tag-${i}`} className="h-11 w-full" />
					))}
				</div>
			)}
			{!isLoading && tags.length === 0 && <EmptyState title="No tags yet" />}
			<div className="space-y-1">
				{tags.map((t) => {
					const isEditing = editing === t.name;
					return (
						<div
							key={t.name}
							className="flex items-center gap-3 rounded-md border border-border px-3 py-2 text-sm"
						>
							{isEditing ? (
								<Input
									value={draft}
									onChange={(e) => setDraft(e.target.value)}
									onKeyDown={(e) => {
										if (e.key === "Enter") {
											renameTag.mutate({ name: t.name, newName: draft });
											setEditing(null);
										}
										if (e.key === "Escape") setEditing(null);
									}}
									autoFocus
									className="h-8"
								/>
							) : (
								<Link
									to="/bookmarks"
									search={{ tag: t.name }}
									className="min-w-0 flex-1 truncate font-medium hover:underline"
								>
									{t.name}
								</Link>
							)}
							<Badge variant="secondary">{t.count}</Badge>
							{!isEditing && (
								<>
									<Button
										size="icon"
										variant="ghost"
										onClick={() => {
											setEditing(t.name);
											setDraft(t.name);
										}}
										aria-label="Rename"
									>
										<Pencil />
									</Button>
									<Button
										size="icon"
										variant="ghost"
										onClick={() => setDeleteTarget(t.name)}
										aria-label="Delete"
									>
										<Trash2 />
									</Button>
								</>
							)}
						</div>
					);
				})}
			</div>

			<ConfirmDialog
				open={!!deleteTarget}
				onOpenChange={(open) => !open && setDeleteTarget(null)}
				title={`Delete tag "${deleteTarget}"?`}
				description="This removes the tag from every bookmark that has it."
				confirmLabel="Delete"
				onConfirm={async () => {
					if (deleteTarget) await deleteTag.mutateAsync(deleteTarget);
				}}
			/>
		</div>
	);
}
