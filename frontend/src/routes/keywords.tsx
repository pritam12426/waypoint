import { createFileRoute } from "@tanstack/react-router";
import { Compass } from "lucide-react";
import { EmptyState } from "#/components/empty-state";
import { Link } from "#/components/link";
import { Skeleton } from "#/components/ui/skeleton";
import { useKeywords } from "#/lib/api/hooks";

export const Route = createFileRoute("/keywords")({
	component: KeywordsPage,
});

function KeywordsPage() {
	const { data: keywords = [], isLoading } = useKeywords();

	return (
		<div className="mx-auto max-w-xl space-y-4">
			<p className="text-sm text-muted-foreground">
				Type a keyword into your browser's address bar to jump straight to its bookmark.
			</p>
			{isLoading && (
				<div className="space-y-1">
					{Array.from({ length: 5 }).map((_, i) => (
						<Skeleton key={`kw-${i}`} className="h-9 w-full" />
					))}
				</div>
			)}
			{!isLoading && keywords.length === 0 && (
				<EmptyState icon={Compass} title="No keywords set yet" />
			)}
			<div className="space-y-1">
				{keywords.map((kw) => (
					<Link
						key={kw}
						to="/bookmarks"
						search={{ keyword: kw }}
						className="flex items-center gap-2 rounded-md border border-border px-3 py-2 font-mono text-sm hover:bg-accent"
					>
						{kw}
					</Link>
				))}
			</div>
		</div>
	);
}
