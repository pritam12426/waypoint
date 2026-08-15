import type * as React from "react";
import { cn } from "#/lib/utils";

// Card layout primitives (container/header/title/content/footer) for paneled
// sections like the bookmark detail and settings pages.
export function Card({ className, ...props }: React.ComponentPropsWithoutRef<"div">) {
	return (
		<div
			className={cn(
				"rounded-lg border border-border bg-card text-card-foreground shadow-sm",
				className,
			)}
			{...props}
		/>
	);
}

export function CardHeader({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return <div className={cn("flex flex-col gap-1.5 p-6", className)} {...props} />;
}

export function CardTitle({ className, ...props }: React.ComponentPropsWithoutRef<"h3">) {
	return (
		<h3
			className={cn("text-sm font-medium text-muted-foreground", className)}
			{...props}
		/>
	);
}

export function CardDescription({
	className,
	...props
}: React.ComponentPropsWithoutRef<"p">) {
	return <p className={cn("text-sm text-muted-foreground", className)} {...props} />;
}

export function CardContent({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return <div className={cn("p-6 pt-0", className)} {...props} />;
}

export function CardFooter({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return <div className={cn("flex items-center p-6 pt-0", className)} {...props} />;
}
