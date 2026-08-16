import { useNavigate } from "@tanstack/react-router";
import { Command } from "cmdk";
import {
	Archive,
	Dices,
	Link2,
	PieChart,
	Plus,
	Search,
	Settings,
	Star,
	Tags,
	TextCursorInput,
	Trash2,
} from "lucide-react";
import { useState } from "react";
import { Favicon } from "#/components/bookmark-media";
import { useDebouncedValue } from "#/hooks/use-debounced-value";
import { useSearch } from "#/lib/api/hooks";

export interface CommandPaletteProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

const NAV_ACTIONS = [
	{ to: "/overview", label: "Go to Stats", icon: PieChart },
	{ to: "/bookmarks", label: "Go to Bookmarks", icon: Link2 },
	{ to: "/starred", label: "Go to Starred", icon: Star },
	{ to: "/archived", label: "Go to Archived", icon: Archive },
	{ to: "/random", label: "Go to Random", icon: Dices },
	{ to: "/bookmarks/new", label: "New bookmark", icon: Plus },
	{ to: "/search", label: "Go to Search", icon: Search },
	{ to: "/trash", label: "Go to Trash", icon: Trash2 },
	{ to: "/categories", label: "Go to Categories", icon: Archive },
	{ to: "/tags", label: "Go to Tags", icon: Tags },
	{ to: "/keywords", label: "Go to Keywords", icon: TextCursorInput },
	{ to: "/settings", label: "Go to Settings", icon: Settings },
] as const;

export function CommandPalette({ open, onOpenChange }: CommandPaletteProps) {
	const navigate = useNavigate();
	const [query, setQuery] = useState("");
	const debouncedQuery = useDebouncedValue(query, 200);
	const { data } = useSearch(debouncedQuery, { limit: 8 });

	function go(to: string) {
		onOpenChange(false);
		setQuery("");
		navigate({ to });
	}

	return (
		<Command.Dialog
			open={open}
			onOpenChange={onOpenChange}
			label="Command palette"
			className="fixed left-1/2 top-24 z-50 w-full max-w-lg -translate-x-1/2 overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg"
		>
			<Command.Input
				value={query}
				onValueChange={setQuery}
				placeholder="Search bookmarks or jump to a page…"
				className="w-full border-b border-border bg-transparent px-4 py-3 text-sm outline-none placeholder:text-muted-foreground"
			/>
			<Command.List className="max-h-96 overflow-y-auto p-2">
				<Command.Empty className="py-6 text-center text-sm text-muted-foreground">
					No results.
				</Command.Empty>

				{query.trim() === "" && (
					<Command.Group
						heading="Navigate"
						className="px-2 py-1 text-xs text-muted-foreground"
					>
						{NAV_ACTIONS.map((item) => (
							<Command.Item
								key={item.to}
								onSelect={() => go(item.to)}
								className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm text-foreground data-[selected=true]:bg-accent"
							>
								<item.icon className="size-4" />
								{item.label}
							</Command.Item>
						))}
					</Command.Group>
				)}

				{query.trim() !== "" && data && data.items.length > 0 && (
					<Command.Group
						heading="Bookmarks"
						className="px-2 py-1 text-xs text-muted-foreground"
					>
						{data.items.map((bm) => (
							<Command.Item
								key={bm.id}
								onSelect={() => go(`/bookmarks/${bm.id}`)}
								className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm text-foreground data-[selected=true]:bg-accent"
							>
								<Favicon src={bm.favicon} domain={bm.domain} />
								<span className="truncate">{bm.title || bm.url}</span>
							</Command.Item>
						))}
					</Command.Group>
				)}
			</Command.List>
		</Command.Dialog>
	);
}
