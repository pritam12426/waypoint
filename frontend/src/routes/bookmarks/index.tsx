import { createFileRoute } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
	Archive,
	ArchiveRestore,
	ExternalLink,
	MoreHorizontal,
	Pencil,
	Plus,
	Star,
	Tag as TagIcon,
	Trash2,
} from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { z } from "zod";
import { Favicon } from "#/components/bookmark-media";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { EmptyState } from "#/components/empty-state";
import { Kbd } from "#/components/kbd";
import { Link } from "#/components/link";
import { TagsInput } from "#/components/tags-input";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "#/components/ui/dialog";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Skeleton } from "#/components/ui/skeleton";
import { useListNavigation } from "#/hooks/use-list-navigation";
import {
	useBookmarks,
	useBulkDeleteBookmarks,
	useBulkUpdateBookmarks,
	useDeleteBookmark,
	useUpdateBookmark,
} from "#/lib/api/hooks";
import { formatRelative } from "#/lib/format";
import { useListNav } from "#/lib/list-nav";

const searchSchema = z.object({
	category: z.string().optional(),
	tag: z.string().optional(),
	keyword: z.string().optional(),
	starred: z.boolean().optional(),
	archived: z.boolean().optional(),
	page: z.number().int().min(0).optional().default(0),
});

const PAGE_SIZE = 50;

export const Route = createFileRoute("/bookmarks/")({
	validateSearch: searchSchema,
	component: BookmarksListPage,
});

function BookmarksListPage() {
	const search = Route.useSearch();
	const navigate = Route.useNavigate();
	const page = search.page ?? 0;

	const { data, isLoading } = useBookmarks({
		category: search.category,
		tag: search.tag,
		keyword: search.keyword,
		starred: search.starred,
		archived: search.archived,
		limit: PAGE_SIZE,
		offset: page * PAGE_SIZE,
	});

	const items = data?.items ?? [];
	const total = data?.total ?? 0;

	const [selected, setSelected] = useState<Set<number>>(new Set());
	const [bulkTagsOpen, setBulkTagsOpen] = useState(false);
	const [bulkTags, setBulkTags] = useState<string[]>([]);
	const [confirmTrash, setConfirmTrash] = useState(false);
	const [confirmDelete, setConfirmDelete] = useState(false);

	const updateBookmark = useUpdateBookmark();
	const deleteBookmark = useDeleteBookmark();
	const bulkUpdate = useBulkUpdateBookmarks();
	const bulkDelete = useBulkDeleteBookmarks();

	const ids = useMemo(() => items.map((b) => b.id), [items]);

	function toggleSelect(id: number) {
		setSelected((prev) => {
			const next = new Set(prev);
			next.has(id) ? next.delete(id) : next.add(id);
			return next;
		});
	}

	useListNavigation(ids, {
		onOpen: (id) => window.open(`/open/${id}`, "_blank"),
		onCopyUrl: (id) => {
			const bm = items.find((b) => b.id === id);
			if (bm) navigator.clipboard.writeText(bm.url);
		},
		onToggleSelect: toggleSelect,
		onStar: (id) => {
			const bm = items.find((b) => b.id === id);
			if (bm) updateBookmark.mutate({ id, body: { starred: !bm.starred } });
		},
		onArchive: (id) => {
			const bm = items.find((b) => b.id === id);
			if (bm) updateBookmark.mutate({ id, body: { is_archived: !bm.is_archived } });
		},
		onEdit: (id) => navigate({ to: "/bookmarks/$id/edit", params: { id: String(id) } }),
		onTrash: (id) => deleteBookmark.mutate({ id }),
	});

	const activeId = useListNav((s) => s.activeId);

	const parentRef = useRef<HTMLDivElement>(null);
	const virtualizer = useVirtualizer({
		count: items.length,
		getScrollElement: () => parentRef.current,
		estimateSize: () => 44,
		overscan: 12,
	});

	const filterLabel = [
		search.category && `category: ${search.category}`,
		search.tag && `tag: ${search.tag}`,
		search.keyword && `keyword: ${search.keyword}`,
		search.starred && "starred",
		search.archived && "archived",
	]
		.filter(Boolean)
		.join(" · ");

	return (
		<div className="space-y-4">
			<div className="flex items-center justify-between">
				<div>
					<h1 className="text-xl font-semibold">Bookmarks</h1>
					{filterLabel && <p className="text-sm text-muted-foreground">{filterLabel}</p>}
				</div>
				<Button asChild>
					<Link to="/bookmarks/new">
						<Plus /> New bookmark
					</Link>
				</Button>
			</div>

			{selected.size > 0 && (
				<div className="flex items-center gap-2 rounded-md border border-border bg-card px-3 py-2">
					<span className="text-sm font-medium">{selected.size} selected</span>
					<div className="ml-auto flex gap-2">
						<Button size="sm" variant="outline" onClick={() => setBulkTagsOpen(true)}>
							<TagIcon /> Add tags
						</Button>
						<Button
							size="sm"
							variant="outline"
							onClick={() =>
								bulkUpdate.mutate({ ids: [...selected], update: { is_archived: true } })
							}
						>
							<Archive /> Archive
						</Button>
						<Button size="sm" variant="outline" onClick={() => setConfirmTrash(true)}>
							<Trash2 /> Trash
						</Button>
						<Button
							size="sm"
							variant="destructive"
							onClick={() => setConfirmDelete(true)}
						>
							Delete forever
						</Button>
					</div>
				</div>
			)}

			{isLoading && (
				<div className="space-y-1">
					{Array.from({ length: 10 }).map((_, i) => (
						<Skeleton key={`row-${i}`} className="h-11 w-full" />
					))}
				</div>
			)}

			{!isLoading && items.length === 0 && (
				<EmptyState
					title="No bookmarks match these filters"
					description="Try clearing filters or add a new bookmark."
					action={
						<Button asChild size="sm">
							<Link to="/bookmarks/new">
								<Plus /> New bookmark
							</Link>
						</Button>
					}
				/>
			)}

			{!isLoading && items.length > 0 && (
				<div
					ref={parentRef}
					className="max-h-[70vh] overflow-y-auto rounded-lg border border-border"
				>
					<div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
						{virtualizer.getVirtualItems().map((row) => {
							const bm = items[row.index];
							const isActive = bm.id === activeId;
							return (
								<div
									key={bm.id}
									style={{
										position: "absolute",
										top: 0,
										left: 0,
										width: "100%",
										height: row.size,
										transform: `translateY(${row.start}px)`,
									}}
									className={`flex items-center gap-3 border-b border-border px-3 text-sm ${isActive ? "border-l-2 border-l-primary bg-accent/60" : ""}`}
								>
									<Checkbox
										checked={selected.has(bm.id)}
										onCheckedChange={() => toggleSelect(bm.id)}
									/>
									<Favicon src={bm.favicon} domain={bm.domain} />
									<Link
										to="/bookmarks/$id"
										params={{ id: String(bm.id) }}
										className="min-w-0 flex-1 truncate font-medium hover:underline"
									>
										{bm.title || bm.url}
									</Link>
									<span className="hidden shrink-0 text-xs text-muted-foreground sm:inline">
										{bm.domain}
									</span>
									{bm.starred && (
										<Star className="size-3.5 shrink-0 fill-primary text-primary" />
									)}
									{bm.tags.slice(0, 3).map((t) => (
										<Badge
											key={t}
											variant="secondary"
											className="hidden shrink-0 lg:inline-flex"
										>
											{t}
										</Badge>
									))}
									<span className="hidden shrink-0 text-xs text-muted-foreground md:inline">
										{formatRelative(bm.created_at)}
									</span>
									<DropdownMenu>
										<DropdownMenuTrigger asChild>
											<Button variant="ghost" size="icon" className="shrink-0">
												<MoreHorizontal />
											</Button>
										</DropdownMenuTrigger>
										<DropdownMenuContent align="end">
											<DropdownMenuItem
												onClick={() => window.open(`/open/${bm.id}`, "_blank")}
											>
												<ExternalLink /> Open
											</DropdownMenuItem>
											<DropdownMenuItem
												onClick={() =>
													updateBookmark.mutate({
														id: bm.id,
														body: { starred: !bm.starred },
													})
												}
											>
												<Star /> {bm.starred ? "Unstar" : "Star"}
											</DropdownMenuItem>
											<DropdownMenuItem
												onClick={() =>
													updateBookmark.mutate({
														id: bm.id,
														body: { is_archived: !bm.is_archived },
													})
												}
											>
												{bm.is_archived ? <ArchiveRestore /> : <Archive />}
												{bm.is_archived ? "Unarchive" : "Archive"}
											</DropdownMenuItem>
											<DropdownMenuItem
												onClick={() =>
													navigate({
														to: "/bookmarks/$id/edit",
														params: { id: String(bm.id) },
													})
												}
											>
												<Pencil /> Edit
											</DropdownMenuItem>
											<DropdownMenuItem
												className="text-destructive"
												onClick={() => deleteBookmark.mutate({ id: bm.id })}
											>
												<Trash2 /> Trash
											</DropdownMenuItem>
										</DropdownMenuContent>
									</DropdownMenu>
								</div>
							);
						})}
					</div>
				</div>
			)}

			{total > PAGE_SIZE && (
				<div className="flex items-center justify-between text-sm text-muted-foreground">
					<span>
						{page * PAGE_SIZE + 1}–{Math.min(total, (page + 1) * PAGE_SIZE)} of {total}
					</span>
					<div className="flex gap-2">
						<Button
							variant="outline"
							size="sm"
							disabled={page === 0}
							onClick={() => navigate({ search: { ...search, page: page - 1 } })}
						>
							Previous
						</Button>
						<Button
							variant="outline"
							size="sm"
							disabled={(page + 1) * PAGE_SIZE >= total}
							onClick={() => navigate({ search: { ...search, page: page + 1 } })}
						>
							Next
						</Button>
					</div>
				</div>
			)}

			<p className="flex items-center gap-1.5 text-xs text-muted-foreground">
				<Kbd>j</Kbd>
				<Kbd>k</Kbd> move · <Kbd>o</Kbd> open · <Kbd>x</Kbd> select · <Kbd>s</Kbd> star ·{" "}
				<Kbd>a</Kbd> archive · <Kbd>e</Kbd> edit · <Kbd>d</Kbd> trash · press <Kbd>?</Kbd>{" "}
				for more
			</p>

			<Dialog open={bulkTagsOpen} onOpenChange={setBulkTagsOpen}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Add tags to {selected.size} bookmarks</DialogTitle>
					</DialogHeader>
					<TagsInput value={bulkTags} onChange={setBulkTags} />
					<DialogFooter>
						<Button
							onClick={() => {
								bulkUpdate.mutate({ ids: [...selected], update: { tags: bulkTags } });
								setBulkTagsOpen(false);
								setBulkTags([]);
							}}
						>
							Apply
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			<ConfirmDialog
				open={confirmTrash}
				onOpenChange={setConfirmTrash}
				title="Trash selected bookmarks?"
				description={`${selected.size} bookmarks will be moved to trash.`}
				confirmLabel="Trash"
				onConfirm={async () => {
					await bulkDelete.mutateAsync({ ids: [...selected] });
					setSelected(new Set());
				}}
			/>
			<ConfirmDialog
				open={confirmDelete}
				onOpenChange={setConfirmDelete}
				title="Delete forever?"
				description={`${selected.size} bookmarks will be permanently deleted. This cannot be undone.`}
				confirmLabel="Delete forever"
				onConfirm={async () => {
					await bulkDelete.mutateAsync({ ids: [...selected], purge: true });
					setSelected(new Set());
				}}
			/>
		</div>
	);
}
