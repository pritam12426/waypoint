import * as TabsPrimitive from "@radix-ui/react-tabs";
import type * as React from "react";
import { cn } from "#/lib/utils";

export const Tabs = TabsPrimitive.Root;

export function TabsList({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>) {
	return (
		<TabsPrimitive.List
			className={cn(
				"inline-flex h-9 items-center justify-center rounded-lg bg-muted p-1",
				className,
			)}
			{...props}
		/>
	);
}

export function TabsTrigger({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>) {
	return (
		<TabsPrimitive.Trigger
			className={cn(
				"inline-flex items-center justify-center whitespace-nowrap rounded-md px-3 py-1 text-sm font-medium transition-all disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-background data-[state=active]:shadow-sm",
				className,
			)}
			{...props}
		/>
	);
}

export function TabsContent({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>) {
	return (
		<TabsPrimitive.Content
			className={cn("mt-2 focus-visible:outline-none", className)}
			{...props}
		/>
	);
}
