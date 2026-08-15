import { useNavigate } from "@tanstack/react-router";
import {
	Archive,
	Compass,
	Home,
	Keyboard,
	Link2,
	Menu,
	Search,
	Settings,
	Star,
	Tags,
	Trash2,
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { CommandPalette } from "#/components/command-palette";
import { KeyboardHelp } from "#/components/keyboard-help";
import { Link } from "#/components/link";
import { ThemeToggle } from "#/components/theme-toggle";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { useListNav } from "#/lib/list-nav";
import { cn } from "#/lib/utils";

const NAV_ITEMS = [
	{ to: "/", label: "Dashboard", icon: Home },
	{ to: "/bookmarks", label: "Bookmarks", icon: Link2 },
	{ to: "/search", label: "Search", icon: Search },
	{ to: "/trash", label: "Trash", icon: Trash2 },
	{ to: "/categories", label: "Categories", icon: Archive },
	{ to: "/tags", label: "Tags", icon: Tags },
	{ to: "/keywords", label: "Keywords", icon: Compass },
	{ to: "/stats", label: "Stats", icon: Star },
	{ to: "/settings", label: "Settings", icon: Settings },
] as const;

function isTypingTarget(target: EventTarget | null) {
	if (!(target instanceof HTMLElement)) return false;
	const tag = target.tagName.toLowerCase();
	return (
		tag === "input" || tag === "textarea" || tag === "select" || target.isContentEditable
	);
}

export function AppShell({ children }: { children: ReactNode }) {
	const navigate = useNavigate();
	const [paletteOpen, setPaletteOpen] = useState(false);
	const [helpOpen, setHelpOpen] = useState(false);
	const [sidebarOpen, setSidebarOpen] = useState(false);
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
					"fixed inset-y-0 left-0 z-40 w-60 shrink-0 border-r border-border bg-card transition-transform lg:static lg:translate-x-0",
					sidebarOpen ? "translate-x-0" : "-translate-x-full",
				)}
			>
				<div className="flex h-14 items-center gap-2 border-b border-border px-4">
					<Link2 className="size-5 text-primary" />
					<span className="font-semibold">waypointd</span>
				</div>
				<nav className="flex flex-col gap-0.5 p-2">
					{NAV_ITEMS.map((item) => (
						<Link
							key={item.to}
							to={item.to}
							onClick={() => setSidebarOpen(false)}
							className="flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground [&.active]:bg-accent [&.active]:text-accent-foreground"
							activeOptions={{ exact: item.to === "/" }}
						>
							<item.icon className="size-4" />
							{item.label}
						</Link>
					))}
				</nav>
			</aside>

			{sidebarOpen && (
				<button
					type="button"
					aria-label="Close sidebar"
					className="fixed inset-0 z-30 bg-black/50 lg:hidden"
					onClick={() => setSidebarOpen(false)}
				/>
			)}

			<div className="flex min-w-0 flex-1 flex-col">
				<header className="sticky top-0 z-20 flex h-14 items-center gap-3 border-b border-border bg-background/95 px-4 backdrop-blur">
					<Button
						variant="ghost"
						size="icon"
						className="lg:hidden"
						onClick={() => setSidebarOpen(true)}
						aria-label="Open sidebar"
					>
						<Menu />
					</Button>
					<div className="relative max-w-md flex-1">
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
					<div className="ml-auto flex items-center gap-1">
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
		</div>
	);
}
