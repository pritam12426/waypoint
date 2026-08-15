import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { Bar, BarChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { Link } from "#/components/link";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import {
	useStatsActivity,
	useStatsDomains,
	useStatsHygiene,
	useStatsNeverVisited,
	useStatsOrphanTags,
	useStatsOverview,
} from "#/lib/api/hooks";
import { formatDate } from "#/lib/format";

const PAGE_SIZE = 20;

export const Route = createFileRoute("/stats")({
	component: StatsPage,
});

function StatsPage() {
	const { data: overview } = useStatsOverview();
	const { data: activity, isLoading: activityLoading } = useStatsActivity();
	const { data: hygiene } = useStatsHygiene();
	const [domainsPage, setDomainsPage] = useState(0);
	const [neverVisitedPage, setNeverVisitedPage] = useState(0);
	const { data: neverVisited } = useStatsNeverVisited(
		PAGE_SIZE,
		neverVisitedPage * PAGE_SIZE,
	);
	const { data: orphanTags = [] } = useStatsOrphanTags(50, 0);
	const { data: domains } = useStatsDomains(PAGE_SIZE, domainsPage * PAGE_SIZE);

	return (
		<div className="space-y-6">
			<h1 className="text-xl font-semibold">Stats</h1>

			<Card>
				<CardHeader>
					<CardTitle>Activity by month</CardTitle>
				</CardHeader>
				<CardContent className="h-64">
					{activityLoading ? (
						<Skeleton className="h-full w-full" />
					) : (
						<ResponsiveContainer width="100%" height="100%">
							<BarChart data={activity}>
								<XAxis dataKey="month" fontSize={11} stroke="var(--muted-foreground)" />
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
						{overview?.top_domains.slice(0, 8).map((d) => (
							<div key={d.domain} className="flex items-center justify-between text-sm">
								<span className="truncate">{d.domain}</span>
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
						{overview?.top_tags.slice(0, 8).map((t) => (
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
							<span className="min-w-0 flex-1 truncate">{nv.title || nv.url}</span>
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
							<span className="min-w-0 flex-1 truncate">{d.domain}</span>
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

			{orphanTags.length > 0 && (
				<Card>
					<CardHeader>
						<CardTitle>Orphan tags</CardTitle>
					</CardHeader>
					<CardContent className="space-y-1">
						{orphanTags.map((ot) => (
							<Link
								key={`${ot.name}-${ot.bookmark_id}`}
								to="/bookmarks/$id"
								params={{ id: String(ot.bookmark_id) }}
								className="flex items-center justify-between rounded-md px-2 py-1.5 text-sm hover:bg-accent"
							>
								<span className="truncate">{ot.bookmark_title}</span>
								<Badge variant="outline">{ot.name}</Badge>
							</Link>
						))}
					</CardContent>
				</Card>
			)}
		</div>
	);
}
