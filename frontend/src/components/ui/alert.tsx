import { type VariantProps, cva } from "class-variance-authority";
import type * as React from "react";
import { cn } from "#/lib/utils";

const alertVariants = cva(
	"relative w-full rounded-lg border border-border px-4 py-3 text-sm [&>svg]:size-4",
	{
		variants: {
			variant: {
				default: "bg-card text-card-foreground",
				destructive: "border-destructive/50 text-destructive [&>svg]:text-destructive",
			},
		},
		defaultVariants: { variant: "default" },
	},
);

export function Alert({
	className,
	variant,
	...props
}: React.ComponentPropsWithoutRef<"div"> & VariantProps<typeof alertVariants>) {
	return (
		<div role="alert" className={cn(alertVariants({ variant }), className)} {...props} />
	);
}

export function AlertTitle({
	className,
	...props
}: React.ComponentPropsWithoutRef<"h5">) {
	return <h5 className={cn("mb-1 font-medium leading-none", className)} {...props} />;
}

export function AlertDescription({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return (
		<div
			className={cn("text-sm text-muted-foreground [&_p]:leading-relaxed", className)}
			{...props}
		/>
	);
}
