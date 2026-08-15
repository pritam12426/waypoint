import { createFileRoute } from "@tanstack/react-router";
import { Search as SearchIcon } from "lucide-react";
import { useState } from "react";
import { z } from "zod";
import { Favicon } from "#/components/bookmark-media";
import { EmptyState } from "#/components/empty-state";
import { Link } from "#/components/link";
import { Badge } from "#/components/ui/badge";
import { Input } from "#/components/ui/input";
import { Skeleton } from "#/components/ui/skeleton";
import { useDebouncedValue } from "#/hooks/use-debounced-value";
import { useSearch } from "#/lib/api/hooks";
import { formatRelative } from "#/lib/format";

const searchSchema = z.object({ q: z.string().optional().default("") });

export const Route = createFileRoute("/search")({
	validateSearch: searchSchema,
	component: SearchPage,
});

function SearchPage() {
	const { q: initialQ } = Route.useSearch();
	const [query, setQuery] = useState(initialQ);
	const debounced = useDebouncedValue(query, 300);
	const { data, isLoading } = useSearch(debounced, { limit: 50 });

	return (
		<div className="mx-auto max-w-2xl space-y-4">
			<div className="relative">
				<SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
				<Input
					autoFocus
					value={query}
					onChange={(e) => setQuery(e.target.value)}
					placeholder="Search bookmarks…"
					className="pl-9"
				/>
			</div>

			{debounced.trim() === "" && (
				<EmptyState
					icon={SearchIcon}
					title="Start typing to search"
					description="Full-text search across titles, descriptions, and notes."
				/>
			)}

			{isLoading && debounced.trim() !== "" && (
				<div className="space-y-2">
					{Array.from({ length: 5 }).map((_, i) => (
						<Skeleton key={`s-${i}`} className="h-14 w-full" />
					))}
				</div>
			)}

			{data && data.items.length === 0 && debounced.trim() !== "" && (
				<EmptyState title="No results" description={`Nothing matches "${debounced}"`} />
			)}

			{data && data.items.length > 0 && (
				<>
					<p className="text-sm text-muted-foreground">
						{data.total} result{data.total === 1 ? "" : "s"}
					</p>
					<div className="space-y-1">
						{data.items.map((bm) => (
							<Link
								key={bm.id}
								to="/bookmarks/$id"
								params={{ id: String(bm.id) }}
								className="flex items-center gap-3 rounded-md border border-border px-3 py-2.5 text-sm hover:bg-accent"
							>
								<Favicon src={bm.favicon} domain={bm.domain} />
								<span className="min-w-0 flex-1 truncate font-medium">
									{bm.title || bm.url}
								</span>
								{bm.starred && <Badge variant="secondary">starred</Badge>}
								<span className="shrink-0 text-xs text-muted-foreground">
									{formatRelative(bm.created_at)}
								</span>
							</Link>
						))}
					</div>
				</>
			)}
		</div>
	);
}
