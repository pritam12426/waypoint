import { type LinkComponentProps, Link as RouterLink } from "@tanstack/react-router";
import { cn } from "#/lib/utils";

// App-wide link wrapper: every internal link gets the same styling and sets
// aria-current="page" on the active route (drives nav highlighting).
export function Link({ className, activeProps, ...props }: LinkComponentProps) {
	return (
		<RouterLink
			className={cn("transition-colors", className)}
			activeProps={{ "aria-current": "page", ...activeProps } as never}
			{...props}
		/>
	);
}
