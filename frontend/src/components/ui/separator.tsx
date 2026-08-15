import * as SeparatorPrimitive from "@radix-ui/react-separator";
import type * as React from "react";
import { cn } from "#/lib/utils";

// Thin divider between sections. decorative=true keeps it out of the a11y tree.
export function Separator({
	className,
	orientation = "horizontal",
	decorative = true,
	...props
}: React.ComponentPropsWithoutRef<typeof SeparatorPrimitive.Root>) {
	return (
		<SeparatorPrimitive.Root
			orientation={orientation}
			decorative={decorative}
			className={cn(
				"shrink-0 bg-border",
				orientation === "horizontal" ? "h-px w-full" : "h-full w-px",
				className,
			)}
			{...props}
		/>
	);
}
