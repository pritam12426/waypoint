import { Kbd } from "#/components/kbd";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogHeader,
	DialogTitle,
} from "#/components/ui/dialog";

const GLOBAL_SHORTCUTS: [string[], string][] = [
	[["⌘", "K"], "Open command palette"],
	[["t"], "Open command palette"],
	[["?"], "Show this help"],
	[["/"], "Focus search"],
];

const LIST_SHORTCUTS: [string[], string][] = [
	[["j"], "Move down"],
	[["k"], "Move up"],
	[["g", "g"], "Jump to first"],
	[["G"], "Jump to last"],
	[["o"], "Open in new tab"],
	[["Enter"], "Open in new tab"],
	[["Y"], "Copy URL"],
	[["x"], "Toggle selection"],
	[["s"], "Star"],
	[["a"], "Archive"],
	[["e"], "Edit"],
	[["d"], "Trash"],
];

export interface KeyboardHelpProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function KeyboardHelp({ open, onOpenChange }: KeyboardHelpProps) {
	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Keyboard shortcuts</DialogTitle>
					<DialogDescription>
						Shortcuts are ignored while typing in an input, textarea, select, or
						contenteditable field.
					</DialogDescription>
				</DialogHeader>
				<div className="grid gap-6 sm:grid-cols-2">
					<div>
						<h3 className="mb-2 text-xs font-medium uppercase text-muted-foreground">
							Global
						</h3>
						<ul className="space-y-2">
							{GLOBAL_SHORTCUTS.map(([keys, label]) => (
								<li key={label} className="flex items-center justify-between text-sm">
									<span>{label}</span>
									<span className="flex gap-1">
										{keys.map((k) => (
											<Kbd key={k}>{k}</Kbd>
										))}
									</span>
								</li>
							))}
						</ul>
					</div>
					<div>
						<h3 className="mb-2 text-xs font-medium uppercase text-muted-foreground">
							List navigation
						</h3>
						<ul className="space-y-2">
							{LIST_SHORTCUTS.map(([keys, label]) => (
								<li key={label} className="flex items-center justify-between text-sm">
									<span>{label}</span>
									<span className="flex gap-1">
										{keys.map((k, i) => (
											<Kbd key={`${k}-${i}`}>{k}</Kbd>
										))}
									</span>
								</li>
							))}
						</ul>
					</div>
				</div>
			</DialogContent>
		</Dialog>
	);
}
