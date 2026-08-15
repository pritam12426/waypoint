import { Outlet, createRootRoute, useRouter } from "@tanstack/react-router";
import { useEffect } from "react";
import { ErrorBoundary } from "react-error-boundary";
import { Toaster } from "sonner";
import { AppShell } from "#/components/app-shell";
import { ErrorFallback } from "#/components/error-fallback";
import { TooltipProvider } from "#/components/ui/tooltip";
import { onUnauthorized } from "#/lib/api/client";
import { useApp } from "#/lib/state";
import "#/styles.css";

export const Route = createRootRoute({
	component: RootComponent,
});

function RootComponent() {
	const router = useRouter();
	const setToken = useApp((s) => s.setToken);

	useEffect(() => {
		onUnauthorized(() => {
			setToken(null);
			router.navigate({ to: "/settings" });
		});
	}, [router, setToken]);

	return (
		<TooltipProvider>
			<ErrorBoundary FallbackComponent={ErrorFallback}>
				<AppShell>
					<Outlet />
				</AppShell>
			</ErrorBoundary>
			<Toaster richColors closeButton theme="dark" />
		</TooltipProvider>
	);
}
