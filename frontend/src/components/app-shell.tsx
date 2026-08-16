import { useLocation, useNavigate } from "@tanstack/react-router";
import {
	Archive,
	Dices,
	Keyboard,
	Link2,
	Menu,
	PieChart,
	Plus,
	Search,
	Settings,
	Star,
	Tags,
	TextCursorInput,
	Trash2,
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { CommandPalette } from "#/components/command-palette";
import { KeyboardHelp } from "#/components/keyboard-help";
import { Link } from "#/components/link";
import { ThemeToggle } from "#/components/theme-toggle";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "#/components/ui/dialog";
import { Input } from "#/components/ui/input";
import {
	Tooltip,
	TooltipContent,
	TooltipProvider,
	TooltipTrigger,
} from "#/components/ui/tooltip";
import { useListNav } from "#/lib/list-nav";
import { cn } from "#/lib/utils";
import { SettingsContent } from "#/routes/settings";

const NAV_ITEMS = [
	{ to: "/bookmarks", label: "Bookmarks", icon: Link2 },
	{ to: "/starred", label: "Starred", icon: Star },
	{ to: "/archived", label: "Archived", icon: Archive },
	{ to: "/random", label: "Random", icon: Dices },
	{ to: "/overview", label: "Stats", icon: PieChart },
	{ to: "/search", label: "Search", icon: Search },
	{ to: "/trash", label: "Trash", icon: Trash2 },
	{ to: "/categories", label: "Categories", icon: Archive, spacing: true },
	{ to: "/tags", label: "Tags", icon: Tags },
	{ to: "/keywords", label: "Keywords", icon: TextCursorInput },
] as const;

function isTypingTarget(target: EventTarget | null) {
	if (!(target instanceof HTMLElement)) return false;
	const tag = target.tagName.toLowerCase();
	return (
		tag === "input" || tag === "textarea" || tag === "select" || target.isContentEditable
	);
}

const HEADER_TITLES: Record<string, string> = {
	"/bookmarks": "Bookmarks",
	"/starred": "Starred",
	"/archived": "Archived",
	"/random": "Random",
	"/overview": "Stats",
	"/search": "Search",
	"/trash": "Trash",
	"/categories": "Categories",
	"/tags": "Tags",
	"/keywords": "Keywords",
	"/settings": "Settings",
	"/bookmarks/new": "New bookmark",
};

export function AppShell({ children }: { children: ReactNode }) {
	const navigate = useNavigate();
	const location = useLocation();
	const [paletteOpen, setPaletteOpen] = useState(false);
	const [helpOpen, setHelpOpen] = useState(false);
	const [sidebarOpen, setSidebarOpen] = useState(false);
	const [settingsOpen, setSettingsOpen] = useState(false);
	const searchInputRef = useRef<HTMLInputElement>(null);

	// Buffer for the "gg" two-key sequence.
	const lastKeyRef = useRef<{ key: string; at: number } | null>(null);

	useEffect(() => {
		function onKeyDown(e: KeyboardEvent) {
			if (isTypingTarget(e.target)) return;

			// Global shortcuts
			if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
				e.preventDefault();
				setPaletteOpen(true);
				return;
			}
			if (e.key === "t" && !e.metaKey && !e.ctrlKey) {
				e.preventDefault();
				setPaletteOpen(true);
				return;
			}
			if (e.key === "?") {
				e.preventDefault();
				setHelpOpen(true);
				return;
			}
			if (e.key === "/") {
				e.preventDefault();
				searchInputRef.current?.focus();
				return;
			}

			// List navigation
			const { ids, activeId, actions, move } = useListNav.getState();
			if (ids.length === 0) return;

			if (e.key === "j") {
				e.preventDefault();
				move("down");
				return;
			}
			if (e.key === "k") {
				e.preventDefault();
				move("up");
				return;
			}
			if (e.key === "g") {
				const now = Date.now();
				if (lastKeyRef.current?.key === "g" && now - lastKeyRef.current.at < 500) {
					e.preventDefault();
					move("first");
					lastKeyRef.current = null;
				} else {
					lastKeyRef.current = { key: "g", at: now };
				}
				return;
			}
			if (e.key === "G") {
				e.preventDefault();
				move("last");
				return;
			}
			if (activeId === null) return;
			if (e.key === "o" || e.key === "Enter") {
				e.preventDefault();
				actions.onOpen?.(activeId);
			} else if (e.key === "Y") {
				e.preventDefault();
				actions.onCopyUrl?.(activeId);
				toast.success("URL copied");
			} else if (e.key === "x") {
				e.preventDefault();
				actions.onToggleSelect?.(activeId);
			} else if (e.key === "s") {
				e.preventDefault();
				actions.onStar?.(activeId);
			} else if (e.key === "a") {
				e.preventDefault();
				actions.onArchive?.(activeId);
			} else if (e.key === "e") {
				e.preventDefault();
				actions.onEdit?.(activeId);
			} else if (e.key === "d") {
				e.preventDefault();
				actions.onTrash?.(activeId);
			}
		}

		window.addEventListener("keydown", onKeyDown);
		return () => window.removeEventListener("keydown", onKeyDown);
	}, []);

	return (
		<div className="flex min-h-screen bg-background">
			<aside
				className={cn(
					"fixed inset-y-0 left-0 z-40 flex w-60 shrink-0 flex-col border-r border-border bg-card transition-transform lg:w-16 lg:translate-x-0",
					sidebarOpen ? "translate-x-0" : "-translate-x-full",
				)}
			>
				<div className="flex h-14 items-center gap-2 border-b border-border px-4 lg:justify-center lg:px-0">
					<TooltipProvider delayDuration={1000}>
						<Tooltip>
							<TooltipTrigger asChild>
								<Button
									onClick={() => navigate({ to: "/bookmarks/new" })}
									aria-label="New bookmark"
									className="w-full bg-blue-600 text-white hover:bg-blue-700 lg:h-9 lg:w-9 lg:shrink-0 lg:justify-center lg:p-0"
								>
									<Plus className="size-4 shrink-0" />
									<span className="lg:hidden">New bookmark</span>
								</Button>
							</TooltipTrigger>
							<TooltipContent side="right">New bookmark</TooltipContent>
						</Tooltip>
					</TooltipProvider>
				</div>
				<nav className="flex flex-1 flex-col gap-0.5 overflow-y-auto p-2">
					<TooltipProvider delayDuration={1000}>
						{NAV_ITEMS.map((item) => (
							<Tooltip key={item.to}>
								<TooltipTrigger asChild>
									<Link
										to={item.to}
										onClick={() => setSidebarOpen(false)}
										className={`flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground lg:justify-center lg:px-0 [&.active]:bg-accent [&.active]:text-accent-foreground ${"spacing" in item ? "mt-2" : ""}`}
									>
										<item.icon className="size-4 shrink-0" />
										<span className="lg:hidden">{item.label}</span>
									</Link>
								</TooltipTrigger>
								<TooltipContent side="right">{item.label}</TooltipContent>
							</Tooltip>
						))}
					</TooltipProvider>
				</nav>
				<div className="border-t border-border p-2">
					<TooltipProvider delayDuration={1000}>
						<Tooltip>
							<TooltipTrigger asChild>
								<Button
									variant="ghost"
									onClick={() => setSettingsOpen(true)}
									className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground lg:justify-center lg:px-0"
								>
									<Settings className="size-4 shrink-0" />
									<span className="lg:hidden">Settings</span>
								</Button>
							</TooltipTrigger>
							<TooltipContent side="right">Settings</TooltipContent>
						</Tooltip>
					</TooltipProvider>
				</div>
			</aside>

			{sidebarOpen && (
				<button
					type="button"
					aria-label="Close sidebar"
					className="fixed inset-0 z-30 bg-black/50 lg:hidden"
					onClick={() => setSidebarOpen(false)}
				/>
			)}

			<div className="flex min-w-0 flex-1 flex-col lg:ml-16">
				<header className="sticky top-0 z-20 grid h-14 grid-cols-[1fr_auto_1fr] items-center gap-3 border-b border-border bg-background/95 px-4 backdrop-blur">
					<div className="flex min-w-0 items-center gap-2">
						<Button
							variant="ghost"
							size="icon"
							className="lg:hidden"
							onClick={() => setSidebarOpen(true)}
							aria-label="Open sidebar"
						>
							<Menu />
						</Button>
						{HEADER_TITLES[location.pathname] && (
							<h1 className="truncate text-xl font-semibold">
								{HEADER_TITLES[location.pathname]}
							</h1>
						)}
					</div>
					<div className="relative w-full max-w-md">
						<Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
						<Input
							ref={searchInputRef}
							placeholder="Search bookmarks…  (press /)"
							className="pl-8"
							onKeyDown={(e) => {
								if (e.key === "Enter") {
									const q = e.currentTarget.value.trim();
									if (q) navigate({ to: "/search", search: { q } });
								}
							}}
						/>
					</div>
					<div className="flex items-center justify-end gap-1">
						<Button
							variant="ghost"
							size="icon"
							onClick={() => setHelpOpen(true)}
							aria-label="Keyboard shortcuts"
						>
							<Keyboard />
						</Button>
						<ThemeToggle />
					</div>
				</header>

				<main className="flex-1 p-4 lg:p-6">{children}</main>
			</div>

			<CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
			<KeyboardHelp open={helpOpen} onOpenChange={setHelpOpen} />
			<Dialog open={settingsOpen} onOpenChange={setSettingsOpen}>
				<DialogContent className="max-h-[80vh] overflow-y-auto">
					<DialogTitle className="sr-only">Settings</DialogTitle>
					<SettingsContent />
				</DialogContent>
			</Dialog>
		</div>
	);
}
