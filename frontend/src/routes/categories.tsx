import { createFileRoute } from "@tanstack/react-router";
import { Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Link } from "#/components/link";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Skeleton } from "#/components/ui/skeleton";
import {
	useCategories,
	useDeleteCategory,
	useRenameCategory,
	useStatsOverview,
} from "#/lib/api/hooks";

export const Route = createFileRoute("/categories")({
	component: CategoriesPage,
});

function CategoriesPage() {
	const { data: categories = [], isLoading } = useCategories();
	const { data: stats } = useStatsOverview();
	const renameCategory = useRenameCategory();
	const deleteCategory = useDeleteCategory();
	const [editingId, setEditingId] = useState<number | null>(null);
	const [draft, setDraft] = useState("");
	const [deleteTarget, setDeleteTarget] = useState<{ id: number; name: string } | null>(
		null,
	);
	void stats;

	return (
		<div className="mx-auto max-w-xl space-y-4">
			<h1 className="text-xl font-semibold">Categories</h1>
			{isLoading && (
				<div className="space-y-1">
					{Array.from({ length: 4 }).map((_, i) => (
						<Skeleton key={`c-${i}`} className="h-11 w-full" />
					))}
				</div>
			)}
			<div className="space-y-1">
				{categories.map((c) => {
					const isDefault = c.name === "Uncategorized";
					const isEditing = editingId === c.id;
					return (
						<div
							key={c.id}
							className="flex items-center gap-3 rounded-md border border-border px-3 py-2 text-sm"
						>
							{isEditing ? (
								<Input
									value={draft}
									onChange={(e) => setDraft(e.target.value)}
									onKeyDown={(e) => {
										if (e.key === "Enter") {
											renameCategory.mutate({ id: c.id, name: draft });
											setEditingId(null);
										}
										if (e.key === "Escape") setEditingId(null);
									}}
									autoFocus
									className="h-8"
								/>
							) : (
								<Link
									to="/bookmarks"
									search={{ category: c.name }}
									className="min-w-0 flex-1 truncate font-medium hover:underline"
								>
									{c.name}
								</Link>
							)}
							{!isDefault && !isEditing && (
								<>
									<Button
										size="icon"
										variant="ghost"
										onClick={() => {
											setEditingId(c.id);
											setDraft(c.name);
										}}
										aria-label="Rename"
									>
										<Pencil />
									</Button>
									<Button
										size="icon"
										variant="ghost"
										onClick={() => setDeleteTarget({ id: c.id, name: c.name })}
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
				title={`Delete "${deleteTarget?.name}"?`}
				description="Bookmarks in this category move to Uncategorized."
				confirmLabel="Delete"
				onConfirm={async () => {
					if (deleteTarget) await deleteCategory.mutateAsync(deleteTarget.id);
				}}
			/>
		</div>
	);
}
