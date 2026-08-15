import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import type * as React from "react";
import { cn } from "#/lib/utils";

// Hover/focus tooltip primitives. Provider must wrap the tree that uses them.
export const TooltipProvider = TooltipPrimitive.Provider;
export const Tooltip = TooltipPrimitive.Root;
export const TooltipTrigger = TooltipPrimitive.Trigger;

export function TooltipContent({
	className,
	sideOffset = 4,
	...props
}: React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>) {
	return (
		<TooltipPrimitive.Portal>
			<TooltipPrimitive.Content
				sideOffset={sideOffset}
				className={cn(
					"z-50 overflow-hidden rounded-md border border-border bg-popover px-3 py-1.5 text-xs text-popover-foreground shadow-md",
					className,
				)}
				{...props}
			/>
		</TooltipPrimitive.Portal>
	);
}
