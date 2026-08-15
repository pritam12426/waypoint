import { AlertTriangle } from "lucide-react";
import type { FallbackProps } from "react-error-boundary";
import { Button } from "#/components/ui/button";

// React-Error-Boundary fallback for route crashes: shows the error message and
// a "Try again" button wired to resetErrorBoundary.
export function ErrorFallback({ error, resetErrorBoundary }: FallbackProps) {
	const message = error instanceof Error ? error.message : "Something went wrong.";
	return (
		<div className="flex min-h-[60vh] flex-col items-center justify-center gap-4 px-6 text-center">
			<AlertTriangle className="size-10 text-destructive" />
			<div className="space-y-1">
				<p className="text-lg font-semibold">This view crashed</p>
				<p className="max-w-md text-sm text-muted-foreground">{message}</p>
			</div>
			<Button onClick={resetErrorBoundary}>Try again</Button>
		</div>
	);
}
