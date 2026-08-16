import { Check, Monitor, Moon, Sun } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import { useApp } from "#/lib/state";

const OPTIONS = [
	{ value: "light", label: "Light", icon: Sun },
	{ value: "dark", label: "Dark", icon: Moon },
	{ value: "system", label: "System", icon: Monitor },
] as const;

export function ThemeToggle() {
	const theme = useApp((s) => s.theme);
	const setTheme = useApp((s) => s.setTheme);
	const Icon = OPTIONS.find((o) => o.value === theme)?.icon ?? Monitor;

	return (
		<DropdownMenu>
			<Tooltip>
				<TooltipTrigger asChild>
					<DropdownMenuTrigger asChild>
						<Button variant="ghost" size="icon" aria-label="Theme">
							<Icon />
						</Button>
					</DropdownMenuTrigger>
				</TooltipTrigger>
				<TooltipContent>Theme</TooltipContent>
			</Tooltip>
			<DropdownMenuContent align="end">
				{OPTIONS.map((o) => (
					<DropdownMenuItem key={o.value} onClick={() => setTheme(o.value)}>
						<o.icon className="size-4" />
						{o.label}
						{theme === o.value && <Check className="ml-auto size-4" />}
					</DropdownMenuItem>
				))}
			</DropdownMenuContent>
		</DropdownMenu>
	);
}
