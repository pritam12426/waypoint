import type * as React from "react";
import { cn } from "#/lib/utils";

// Styled keyboard-key label for shortcut hints (e.g. "⌘ K").
export function Kbd({ className, ...props }: React.ComponentPropsWithoutRef<"kbd">) {
	return (
		<kbd
			className={cn(
				"inline-flex h-5 min-w-5 items-center justify-center rounded border border-border bg-muted px-1.5 font-mono text-[11px] font-medium text-muted-foreground",
				className,
			)}
			{...props}
		/>
	);
}
