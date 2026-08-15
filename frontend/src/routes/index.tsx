import { createFileRoute } from "@tanstack/react-router";
import { Archive, ArrowRight, Star, Trash2, TrendingUp } from "lucide-react";
import { Favicon } from "#/components/bookmark-media";
import { Link } from "#/components/link";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { ApiError } from "#/lib/api/client";
import { useStatsOverview } from "#/lib/api/hooks";

export const Route = createFileRoute("/")({
	component: DashboardPage,
});

function DashboardPage() {
	const { data, isLoading, error } = useStatsOverview();

	if (error) {
		return (
			<Alert variant="destructive">
				<AlertTitle>Couldn't load stats</AlertTitle>
				<AlertDescription>
					{error instanceof ApiError && error.code === "unauthorized"
						? "This server requires a token. Add it in Settings."
						: (error as Error).message}
				</AlertDescription>
			</Alert>
		);
	}

	const cards = [
		{ label: "Total", value: data?.total, to: "/bookmarks", icon: TrendingUp },
		{
			label: "Starred",
			value: data?.starred,
			to: "/bookmarks",
			search: { starred: true },
			icon: Star,
		},
		{
			label: "Archived",
			value: data?.archived,
			to: "/bookmarks",
			search: { archived: true },
			icon: Archive,
		},
		{ label: "Trashed", value: data?.trashed, to: "/trash", icon: Trash2 },
	] as const;

	return (
		<div className="space-y-6">
			<div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
				{cards.map((c) => (
					<Link key={c.label} to={c.to} search={"search" in c ? c.search : undefined}>
						<Card className="transition-colors hover:border-primary/50">
							<CardHeader className="flex-row items-center justify-between space-y-0 pb-2">
								<CardTitle>{c.label}</CardTitle>
								<c.icon className="size-4 text-muted-foreground" />
							</CardHeader>
							<CardContent>
								{isLoading ? (
									<Skeleton className="h-8 w-16" />
								) : (
									<p className="text-2xl font-semibold">{c.value}</p>
								)}
							</CardContent>
						</Card>
					</Link>
				))}
			</div>

			<div className="grid gap-4 lg:grid-cols-2">
				<Card>
					<CardHeader>
						<CardTitle className="flex items-center justify-between">
							Recently added
							<Link to="/bookmarks" className="text-xs text-primary hover:underline">
								View all <ArrowRight className="inline size-3" />
							</Link>
						</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1">
						{isLoading &&
							Array.from({ length: 5 }).map((_, i) => (
								<Skeleton key={`ra-${i}`} className="h-8 w-full" />
							))}
						{data?.recently_added.slice(0, 8).map((bm) => (
							<Link
								key={bm.id}
								to="/bookmarks/$id"
								params={{ id: String(bm.id) }}
								className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
							>
								<Favicon src={bm.favicon} domain={bm.domain} />
								<span className="truncate">{bm.title || bm.url}</span>
							</Link>
						))}
					</CardContent>
				</Card>

				<Card>
					<CardHeader>
						<CardTitle className="flex items-center justify-between">
							Most visited
							<Link to="/bookmarks" className="text-xs text-primary hover:underline">
								View all <ArrowRight className="inline size-3" />
							</Link>
						</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1">
						{isLoading &&
							Array.from({ length: 5 }).map((_, i) => (
								<Skeleton key={`mv-${i}`} className="h-8 w-full" />
							))}
						{data?.most_visited.slice(0, 8).map((bm) => (
							<Link
								key={bm.id}
								to="/bookmarks/$id"
								params={{ id: String(bm.id) }}
								className="flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
							>
								<span className="flex min-w-0 items-center gap-2">
									<Favicon src={bm.favicon} domain={bm.domain} />
									<span className="truncate">{bm.title || bm.url}</span>
								</span>
								<span className="shrink-0 text-xs text-muted-foreground">
									{bm.visit_count}
								</span>
							</Link>
						))}
					</CardContent>
				</Card>
			</div>
		</div>
	);
}
