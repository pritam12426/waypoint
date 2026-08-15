import { Toaster as Sonner } from "sonner";

// Toast host for the sonner toasts fired via toast() in the app.
export function Toaster(props: React.ComponentPropsWithoutRef<typeof Sonner>) {
	return (
		<Sonner
			theme="dark"
			richColors
			closeButton
			toastOptions={{
				classNames: {
					toast: "bg-popover text-popover-foreground border border-border",
				},
			}}
			{...props}
		/>
	);
}
