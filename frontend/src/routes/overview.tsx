import { createFileRoute } from "@tanstack/react-router";
import { Archive, ArrowRight, Star, Trash2, TrendingUp } from "lucide-react";
import { useState } from "react";
import { Bar, BarChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { Favicon } from "#/components/bookmark-media";
import { Link } from "#/components/link";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { ApiError } from "#/lib/api/client";
import {
	useStatsActivity,
	useStatsDomains,
	useStatsHygiene,
	useStatsInactive,
	useStatsNeverVisited,
	useStatsOverview,
} from "#/lib/api/hooks";
import type { ActivityGranularity } from "#/lib/api/types";
import { formatDate, formatRelative } from "#/lib/format";

const PAGE_SIZE = 20;

export const Route = createFileRoute("/overview")({
	component: OverviewPage,
});

function OverviewPage() {
	const { data, isLoading, error } = useStatsOverview();
	const [granularity, setGranularity] = useState<ActivityGranularity>("month");
	const { data: activity, isLoading: activityLoading } = useStatsActivity(granularity);
	const { data: hygiene } = useStatsHygiene();
	const [domainsPage, setDomainsPage] = useState(0);
	const [neverVisitedPage, setNeverVisitedPage] = useState(0);
	const { data: neverVisited } = useStatsNeverVisited(
		PAGE_SIZE,
		neverVisitedPage * PAGE_SIZE,
	);
	const { data: inactive = [] } = useStatsInactive(50, 0);
	const { data: domains } = useStatsDomains(PAGE_SIZE, domainsPage * PAGE_SIZE);

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
		{ label: "Starred", value: data?.starred, to: "/starred", icon: Star },
		{ label: "Archived", value: data?.archived, to: "/archived", icon: Archive },
		{ label: "Trashed", value: data?.trashed, to: "/trash", icon: Trash2 },
	] as const;

	return (
		<div className="space-y-6">
			<div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
				{cards.map((c) => (
					<Link key={c.label} to={c.to}>
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

			<Card>
				<CardHeader className="flex-row items-center justify-between space-y-0">
					<CardTitle>Activity</CardTitle>
					<div className="flex gap-1">
						{(["day", "month", "year"] as const).map((g) => (
							<Button
								key={g}
								size="sm"
								variant={granularity === g ? "default" : "ghost"}
								onClick={() => setGranularity(g)}
								className="capitalize"
							>
								By {g}
							</Button>
						))}
					</div>
				</CardHeader>
				<CardContent className="h-64">
					{activityLoading ? (
						<Skeleton className="h-full w-full" />
					) : (
						<ResponsiveContainer width="100%" height="100%">
							<BarChart data={activity}>
								<XAxis dataKey="period" fontSize={11} stroke="var(--muted-foreground)" />
								<YAxis
									fontSize={11}
									stroke="var(--muted-foreground)"
									allowDecimals={false}
								/>
								<Tooltip
									contentStyle={{
										background: "var(--popover)",
										border: "1px solid var(--border)",
										borderRadius: 8,
										fontSize: 12,
									}}
								/>
								<Bar dataKey="count" fill="var(--primary)" radius={[4, 4, 0, 0]} />
							</BarChart>
						</ResponsiveContainer>
					)}
				</CardContent>
			</Card>

			<div className="grid gap-4 lg:grid-cols-3">
				<Card>
					<CardHeader>
						<CardTitle>Top domains</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1.5">
						{data?.top_domains.slice(0, 8).map((d) => (
							<div key={d.domain} className="flex items-center justify-between text-sm">
								<span className="flex min-w-0 items-center gap-2">
									<Favicon domain={d.domain} />
									<span className="truncate">{d.domain}</span>
								</span>
								<Badge variant="secondary">{d.count}</Badge>
							</div>
						))}
					</CardContent>
				</Card>

				<Card>
					<CardHeader>
						<CardTitle>Top tags</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1.5">
						{data?.top_tags.slice(0, 8).map((t) => (
							<Link
								key={t.name}
								to="/bookmarks"
								search={{ tag: t.name }}
								className="flex items-center justify-between text-sm hover:underline"
							>
								<span className="truncate">{t.name}</span>
								<Badge variant="secondary">{t.count}</Badge>
							</Link>
						))}
					</CardContent>
				</Card>

				<Card>
					<CardHeader>
						<CardTitle>Hygiene</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1.5 text-sm">
						<div className="flex justify-between">
							<span className="text-muted-foreground">Missing tags</span>
							<span>{hygiene?.missing_tags ?? "—"}</span>
						</div>
						<div className="flex justify-between">
							<span className="text-muted-foreground">Missing note</span>
							<span>{hygiene?.missing_note ?? "—"}</span>
						</div>
						<div className="flex justify-between">
							<span className="text-muted-foreground">Missing description</span>
							<span>{hygiene?.missing_description ?? "—"}</span>
						</div>
					</CardContent>
				</Card>
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

			<Card>
				<CardHeader>
					<CardTitle>Never visited</CardTitle>
				</CardHeader>
				<CardContent className="space-y-1">
					{neverVisited?.items.map((nv) => (
						<Link
							key={nv.id}
							to="/bookmarks/$id"
							params={{ id: String(nv.id) }}
							className="flex items-center justify-between rounded-md px-2 py-1.5 text-sm hover:bg-accent"
						>
							<span className="flex min-w-0 items-center gap-2">
								<Favicon src={nv.favicon} domain={nv.domain} />
								<span className="min-w-0 flex-1 truncate">{nv.title || nv.url}</span>
							</span>
							<span className="shrink-0 text-xs text-muted-foreground">
								{formatDate(nv.created_at)}
							</span>
						</Link>
					))}
					{neverVisited && neverVisited.total > PAGE_SIZE && (
						<div className="flex justify-end gap-2 pt-2">
							<Button
								size="sm"
								variant="outline"
								disabled={neverVisitedPage === 0}
								onClick={() => setNeverVisitedPage((p) => p - 1)}
							>
								Previous
							</Button>
							<Button
								size="sm"
								variant="outline"
								disabled={(neverVisitedPage + 1) * PAGE_SIZE >= neverVisited.total}
								onClick={() => setNeverVisitedPage((p) => p + 1)}
							>
								Next
							</Button>
						</div>
					)}
				</CardContent>
			</Card>

			<Card>
				<CardHeader>
					<CardTitle>Domains (all)</CardTitle>
				</CardHeader>
				<CardContent className="space-y-1">
					{domains?.items.map((d) => (
						<Link
							key={d.domain}
							to="/bookmarks"
							search={{}}
							className="flex items-center justify-between rounded-md px-2 py-1.5 text-sm hover:bg-accent"
						>
							<span className="flex min-w-0 items-center gap-2">
								<Favicon domain={d.domain} />
								<span className="min-w-0 flex-1 truncate">{d.domain}</span>
							</span>
							<span className="shrink-0 text-xs text-muted-foreground">
								{d.bookmark_count} bookmarks · {d.total_visits} visits
							</span>
						</Link>
					))}
					{domains && domains.total > PAGE_SIZE && (
						<div className="flex justify-end gap-2 pt-2">
							<Button
								size="sm"
								variant="outline"
								disabled={domainsPage === 0}
								onClick={() => setDomainsPage((p) => p - 1)}
							>
								Previous
							</Button>
							<Button
								size="sm"
								variant="outline"
								disabled={(domainsPage + 1) * PAGE_SIZE >= domains.total}
								onClick={() => setDomainsPage((p) => p + 1)}
							>
								Next
							</Button>
						</div>
					)}
				</CardContent>
			</Card>

			{inactive.length > 0 && (
				<Card>
					<CardHeader>
						<CardTitle>Inactive bookmarks</CardTitle>
						<CardDescription>Not visited and not modified for over 6 months</CardDescription>
					</CardHeader>
					<CardContent className="space-y-1">
						{inactive.map((ib) => (
							<Link
								key={ib.id}
								to="/bookmarks/$id"
								params={{ id: String(ib.id) }}
								className="flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
							>
								<span className="flex min-w-0 items-center gap-2">
									<Favicon src={ib.favicon} domain={ib.domain ?? ""} />
									<span className="truncate">{ib.title || ib.url}</span>
								</span>
								<span className="shrink-0 text-xs text-muted-foreground">
									{ib.last_visited_at
										? `Visited ${formatRelative(ib.last_visited_at)}`
										: "Never visited"}
								</span>
							</Link>
						))}
					</CardContent>
				</Card>
			)}
		</div>
	);
}
