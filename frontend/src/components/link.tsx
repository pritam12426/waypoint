import { type LinkComponentProps, Link as RouterLink } from "@tanstack/react-router";
import { cn } from "#/lib/utils";

export function Link({ className, activeProps, ...props }: LinkComponentProps) {
	return (
		<RouterLink
			className={cn("transition-colors", className)}
			activeProps={{ "aria-current": "page", ...activeProps } as never}
			{...props}
		/>
	);
}
