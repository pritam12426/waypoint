import { Moon, Sun } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import { useApp } from "#/lib/state";

// Header sun/moon button that flips the theme stored in the app store.
export function ThemeToggle() {
	const theme = useApp((s) => s.theme);
	const toggleTheme = useApp((s) => s.toggleTheme);

	return (
		<Tooltip>
			<TooltipTrigger asChild>
				<Button
					variant="ghost"
					size="icon"
					onClick={toggleTheme}
					aria-label="Toggle theme"
				>
					{theme === "dark" ? <Sun /> : <Moon />}
				</Button>
			</TooltipTrigger>
			<TooltipContent>
				Switch to {theme === "dark" ? "light" : "dark"} theme
			</TooltipContent>
		</Tooltip>
	);
}
