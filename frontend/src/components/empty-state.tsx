import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "#/lib/utils";

export interface EmptyStateProps {
	icon?: LucideIcon;
	title: string;
	description?: string;
	action?: ReactNode;
	className?: string;
}

export function EmptyState({
	icon: Icon,
	title,
	description,
	action,
	className,
}: EmptyStateProps) {
	return (
		<div
			className={cn(
				"flex flex-col items-center justify-center gap-3 rounded-lg border border-dashed border-border px-6 py-16 text-center",
				className,
			)}
		>
			{Icon && <Icon className="size-8 text-muted-foreground" />}
			<div className="space-y-1">
				<p className="text-sm font-medium">{title}</p>
				{description && <p className="text-sm text-muted-foreground">{description}</p>}
			</div>
			{action}
		</div>
	);
}
