import { type LinkComponentProps, Link as RouterLink } from "@tanstack/react-router";
import { cn } from "#/lib/utils";

export function Link({ className, activeProps, ...props }: LinkComponentProps) {
	return (
		<RouterLink
			className={cn("transition-colors", className)}
			// The router's default active styling is `{ className: "active" }`,
			// but it is suppressed the moment `activeProps` is provided — so
			// our defaults carry the `active` class too, or `[&.active]`-
			// styled links (the sidebar) would never light up.
			activeProps={{ "aria-current": "page", className: "active", ...activeProps } as never}
			{...props}
		/>
	);
}
