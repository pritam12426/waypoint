import { createFileRoute } from "@tanstack/react-router";
import {
	ChevronLeft,
	ChevronRight,
	Dices,
	ExternalLink,
	Loader2,
	RotateCcw,
	Shuffle,
} from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Favicon } from "#/components/bookmark-media";
import { EmptyState } from "#/components/empty-state";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { Label } from "#/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "#/components/ui/select";
import { useCategories, useRandomPool, useTags } from "#/lib/api/hooks";
import { bookmarksApi } from "#/lib/api/endpoints";
import type { RandomParams } from "#/lib/api/endpoints";
import type { Bookmark } from "#/lib/api/types";
import { cn } from "#/lib/utils";
import { formatRelative } from "#/lib/format";

export const Route = createFileRoute("/random")({
	component: RandomPage,
});

type Pool = NonNullable<RandomParams["pool"]>;
type Mode = "list" | "swipe";

function RandomPage() {
	const [count, setCount] = useState(5);
	const [category, setCategory] = useState("__all__");
	const [tag, setTag] = useState("__all__");
	const [includeArchived, setIncludeArchived] = useState(false);
	const [starredOnly, setStarredOnly] = useState(false);
	const [pool, setPool] = useState<Pool>("all");
	const [mode, setMode] = useState<Mode>("list");
	const [picks, setPicks] = useState<Bookmark[] | null>(null);
	const [poolSize, setPoolSize] = useState(0);
	const [rolling, setRolling] = useState(false);
	const [swipeIndex, setSwipeIndex] = useState(0);
	const [openingAll, setOpeningAll] = useState(false);

	const { data: categories } = useCategories();
	const { data: tags } = useTags();

	const params: RandomParams = {
		limit: Math.min(100, Math.max(1, count)),
		category: category === "__all__" ? undefined : category,
		tag: tag === "__all__" ? undefined : tag,
		archived: includeArchived ? undefined : false,
		starred: starredOnly || undefined,
		pool: pool === "all" ? undefined : pool,
	};

	const { data: available } = useRandomPool(params);

	async function roll() {
		setRolling(true);
		try {
			const res = await bookmarksApi.random(params);
			setPicks(res.data);
			setPoolSize(Number(res.headers["x-total-count"] ?? res.data.length));
			setSwipeIndex(0);
			if (res.data.length === 0) {
				toast.info("No bookmarks in this pool");
			}
		} catch (err) {
			toast.error(err instanceof Error ? err.message : "Failed to pick bookmarks");
		} finally {
			setRolling(false);
		}
	}

	async function surprise() {
		setRolling(true);
		try {
			const res = await bookmarksApi.random({ ...params, limit: 1 });
			const bm = res.data[0];
			if (bm) {
				window.open(`/open/${bm.id}`, "_blank");
			} else {
				toast.info("No bookmarks in this pool");
			}
		} catch (err) {
			toast.error(err instanceof Error ? err.message : "Failed to pick a bookmark");
		} finally {
			setRolling(false);
		}
	}

	function openAll() {
		if (!picks?.length) return;
		setOpeningAll(true);
		for (let i = 0; i < picks.length; i++) {
			setTimeout(() => window.open(`/open/${picks[i].id}`, "_blank"), i * 2000);
		}
		setTimeout(() => setOpeningAll(false), picks.length * 2000);
	}

	function openBookmark(bm: Bookmark) {
		window.open(`/open/${bm.id}`, "_blank");
	}

	function moveSwipe(dir: 1 | -1) {
		if (!picks?.length) return;
		setSwipeIndex((i) => (i + dir + picks.length) % picks.length);
	}

	const current = picks?.[swipeIndex];
	const filterLabel = [
		category !== "__all__" && `category: ${category}`,
		tag !== "__all__" && `tag: ${tag}`,
		pool !== "all" && `pool: ${pool.replace("_", " ")}`,
	].filter(Boolean);

	return (
		<div className="mx-auto max-w-2xl space-y-4">
			<div className="space-y-3 rounded-lg border border-border bg-card p-4">
				<div className="flex flex-wrap items-end gap-3">
					<div className="space-y-1">
						<Label htmlFor="count">Count</Label>
						<Input
							id="count"
							type="number"
							min={1}
							max={100}
							value={count}
							onChange={(e) => setCount(Number(e.target.value))}
							className="w-24"
						/>
					</div>
					<div className="space-y-1">
						<Label>Category</Label>
						<Select value={category} onValueChange={setCategory}>
							<SelectTrigger className="w-44">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="__all__">All categories</SelectItem>
								{(categories ?? []).map((c) => (
									<SelectItem key={c.id} value={c.name}>
										{c.name}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>
					<div className="space-y-1">
						<Label>Tag</Label>
						<Select value={tag} onValueChange={setTag}>
							<SelectTrigger className="w-44">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="__all__">All tags</SelectItem>
								{(tags ?? []).map((t) => (
									<SelectItem key={t.name} value={t.name}>
										{t.name}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>
					<div className="space-y-1">
						<Label>Pool</Label>
						<Select value={pool} onValueChange={(v) => setPool(v as Pool)}>
							<SelectTrigger className="w-48">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="all">All bookmarks</SelectItem>
								<SelectItem value="never_visited">Never visited</SelectItem>
								<SelectItem value="unseen_90d">Not visited in 90 days</SelectItem>
							</SelectContent>
						</Select>
					</div>
				</div>

				<div className="flex flex-wrap items-center gap-4">
					<div className="flex items-center gap-2">
						<Checkbox
							id="archived"
							checked={includeArchived}
							onCheckedChange={(v) => setIncludeArchived(v === true)}
						/>
						<Label htmlFor="archived">Include archived</Label>
					</div>
					<div className="flex items-center gap-2">
						<Checkbox
							id="starred"
							checked={starredOnly}
							onCheckedChange={(v) => setStarredOnly(v === true)}
						/>
						<Label htmlFor="starred">Starred only</Label>
					</div>
					{filterLabel.length > 0 && (
						<p className="text-xs text-muted-foreground">{filterLabel.join(" · ")}</p>
					)}
					<p className="text-xs text-muted-foreground">
						{available !== undefined ? `${available} available` : "…"}
					</p>
					<div className="ml-auto flex items-center gap-2">
						<Button variant="outline" onClick={surprise} disabled={rolling}>
							<Shuffle className="size-4" /> Surprise me
						</Button>
						<Button onClick={roll} disabled={rolling}>
							{rolling ? <Loader2 className="size-4 animate-spin" /> : <Dices />}
							Roll
						</Button>
					</div>
				</div>
			</div>

			{picks !== null && picks.length > 0 && (
				<div className="flex flex-wrap items-center justify-between gap-2">
					<div className="flex items-center gap-2">
						<p className="text-sm text-muted-foreground">
							Picked {picks.length} from a pool of {poolSize}
						</p>
						<Button variant="ghost" size="sm" onClick={roll} disabled={rolling}>
							<RotateCcw /> Re-roll
						</Button>
					</div>
					<div className="flex items-center gap-2">
						<div className="flex rounded-md border border-border">
							<button
								type="button"
								onClick={() => setMode("list")}
								className={cn(
									"rounded-l-md px-3 py-1.5 text-sm",
									mode === "list" && "bg-accent text-accent-foreground",
								)}
							>
								List
							</button>
							<button
								type="button"
								onClick={() => setMode("swipe")}
								className={cn(
									"rounded-r-md border-l border-border px-3 py-1.5 text-sm",
									mode === "swipe" && "bg-accent text-accent-foreground",
								)}
							>
								Swipe
							</button>
						</div>
						<Button variant="outline" onClick={openAll} disabled={openingAll}>
							<ExternalLink /> Open all (2s apart)
						</Button>
					</div>
				</div>
			)}

			{picks !== null && picks.length === 0 && (
				<EmptyState
					icon={Dices}
					title="No bookmarks found"
					description="Nothing matches these filters — adjust them and roll again."
				/>
			)}

			{picks !== null && mode === "list" && picks.length > 0 && (
				<div className="space-y-1">
					{picks.map((bm) => (
						<button
							key={bm.id}
							type="button"
							onClick={() => openBookmark(bm)}
							className="flex w-full items-center gap-3 rounded-md border border-border px-3 py-2.5 text-left text-sm hover:bg-accent"
						>
							<Favicon src={bm.favicon} domain={bm.domain} />
							<span className="min-w-0 flex-1">
								<span className="block truncate font-medium">{bm.title || bm.url}</span>
								<span className="block truncate text-xs text-muted-foreground">
									{bm.domain}
									{bm.category !== "Uncategorized" && ` · ${bm.category}`}
									{bm.tags.length > 0 && ` · ${bm.tags.join(", ")}`}
								</span>
							</span>
							{bm.starred && <Badge variant="secondary">starred</Badge>}
							{bm.is_archived && <Badge variant="secondary">archived</Badge>}
							<span className="shrink-0 text-xs text-muted-foreground">
								{formatRelative(bm.created_at)}
							</span>
						</button>
					))}
				</div>
			)}

			{picks !== null && mode === "swipe" && current && (
				<div className="space-y-3 rounded-lg border border-border bg-card p-4">
					<div className="flex items-center justify-between text-sm text-muted-foreground">
						<span>
							{swipeIndex + 1} of {picks.length}
						</span>
						<Button variant="outline" size="sm" onClick={openAll} disabled={openingAll}>
							<ExternalLink /> Open all
						</Button>
					</div>
					<div className="space-y-2">
						<div className="flex items-center gap-3">
							<Favicon src={current.favicon} domain={current.domain} />
							<span className="text-lg font-semibold">{current.title || current.url}</span>
						</div>
						<p className="truncate text-sm text-muted-foreground">{current.url}</p>
						<div className="flex flex-wrap gap-1">
							{current.category !== "Uncategorized" && (
								<Badge variant="secondary">{current.category}</Badge>
							)}
							{current.tags.map((t) => (
								<Badge key={t} variant="secondary">
									{t}
								</Badge>
							))}
							{current.starred && <Badge variant="secondary">starred</Badge>}
							{current.is_archived && <Badge variant="secondary">archived</Badge>}
						</div>
						<p className="text-xs text-muted-foreground">
							Created {formatRelative(current.created_at)}
						</p>
					</div>
					<div className="flex items-center justify-between">
						<Button variant="outline" onClick={() => moveSwipe(-1)}>
							<ChevronLeft /> Prev
						</Button>
						<Button onClick={() => openBookmark(current)}>
							<ExternalLink /> Open this
						</Button>
						<Button variant="outline" onClick={() => moveSwipe(1)}>
							Next <ChevronRight />
						</Button>
					</div>
				</div>
			)}
		</div>
	);
}
