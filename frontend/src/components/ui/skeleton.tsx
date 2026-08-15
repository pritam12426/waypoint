import type * as React from "react";
import { cn } from "#/lib/utils";

// Placeholder block that pulses while data is loading; sized by className.
export function Skeleton({ className, ...props }: React.ComponentPropsWithoutRef<"div">) {
	return (
		<div className={cn("animate-pulse rounded-md bg-muted", className)} {...props} />
	);
}
