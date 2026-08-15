import * as AlertDialogPrimitive from "@radix-ui/react-alert-dialog";
import type * as React from "react";
import { buttonVariants } from "#/components/ui/button";
import { cn } from "#/lib/utils";

// Modal for destructive confirmations: focus is trapped in the dialog so the
// cancel/confirm buttons can't be skipped by accident. Action/Cancel reuse the
// Button variants (destructive / outline).
export const AlertDialog = AlertDialogPrimitive.Root;
export const AlertDialogTrigger = AlertDialogPrimitive.Trigger;

export function AlertDialogContent({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Content>) {
	return (
		<AlertDialogPrimitive.Portal>
			<AlertDialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
			<AlertDialogPrimitive.Content
				className={cn(
					"fixed left-1/2 top-1/2 z-50 grid w-full max-w-md -translate-x-1/2 -translate-y-1/2 gap-4 rounded-lg border border-border bg-card p-6 shadow-lg data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
					className,
				)}
				{...props}
			/>
		</AlertDialogPrimitive.Portal>
	);
}

export function AlertDialogHeader({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return <div className={cn("flex flex-col gap-1.5 text-left", className)} {...props} />;
}

export function AlertDialogFooter({
	className,
	...props
}: React.ComponentPropsWithoutRef<"div">) {
	return (
		<div
			className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)}
			{...props}
		/>
	);
}

export function AlertDialogTitle({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Title>) {
	return (
		<AlertDialogPrimitive.Title
			className={cn("text-lg font-semibold", className)}
			{...props}
		/>
	);
}

export function AlertDialogDescription({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Description>) {
	return (
		<AlertDialogPrimitive.Description
			className={cn("text-sm text-muted-foreground", className)}
			{...props}
		/>
	);
}

export function AlertDialogAction({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Action>) {
	return (
		<AlertDialogPrimitive.Action
			className={cn(buttonVariants({ variant: "destructive" }), className)}
			{...props}
		/>
	);
}

export function AlertDialogCancel({
	className,
	...props
}: React.ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Cancel>) {
	return (
		<AlertDialogPrimitive.Cancel
			className={cn(buttonVariants({ variant: "outline" }), className)}
			{...props}
		/>
	);
}
