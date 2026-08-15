import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { Input } from "#/components/ui/input";
import { Label } from "#/components/ui/label";
import { useAuthStatus } from "#/lib/api/hooks";
import { useApp } from "#/lib/state";

export const Route = createFileRoute("/settings")({
	component: SettingsPage,
});

function SettingsPage() {
	const token = useApp((s) => s.token);
	const setToken = useApp((s) => s.setToken);
	const [draft, setDraft] = useState(token ?? "");
	const { data: authStatus, refetch } = useAuthStatus();

	function save() {
		setToken(draft.trim() || null);
		toast.success("Token saved");
		setTimeout(() => refetch(), 100);
	}

	return (
		<div className="mx-auto max-w-lg space-y-6">
			<h1 className="text-xl font-semibold">Settings</h1>

			<Card>
				<CardHeader>
					<CardTitle>Access token</CardTitle>
				</CardHeader>
				<CardContent className="space-y-3">
					<p className="text-sm text-muted-foreground">
						Required only if the server was started with{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">
							WAYPOINTD_SERVE_TOKEN
						</code>{" "}
						or{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">
							WAYPOINTD_READ_TOKEN
						</code>
						. Cleared automatically if a request comes back unauthorized.
					</p>
					<div className="space-y-1.5">
						<Label htmlFor="token">Bearer token</Label>
						<Input
							id="token"
							type="password"
							value={draft}
							onChange={(e) => setDraft(e.target.value)}
							placeholder="Leave empty for an open server"
						/>
					</div>
					<Button onClick={save}>Save token</Button>

					{authStatus && (
						<Alert>
							<AlertTitle>
								{authStatus.authenticated ? "Authenticated" : "Not authenticated"}
							</AlertTitle>
							<AlertDescription>
								{authStatus.read_only ? "Read-only access." : "Full access."}
							</AlertDescription>
						</Alert>
					)}
				</CardContent>
			</Card>

			<Card>
				<CardHeader>
					<CardTitle>API endpoint</CardTitle>
				</CardHeader>
				<CardContent className="space-y-1 text-sm text-muted-foreground">
					<p>
						Origin:{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">
							{window.location.origin}
						</code>
					</p>
					<p>
						In dev, Vite proxies{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">/api</code>,{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">/keywords</code>, and{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">/open</code> to the
						Rust server on{" "}
						<code className="rounded bg-muted px-1 py-0.5 text-xs">
							WAYPOINTD_SERVE_PORT
						</code>{" "}
						(default 8080). In production the binary serves the built SPA directly — no
						proxy involved.
					</p>
				</CardContent>
			</Card>
		</div>
	);
}
