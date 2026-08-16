import { create } from "zustand";
import { persist } from "zustand/middleware";

export type Theme = "dark" | "light" | "system";

function prefersDark() {
	return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function applyTheme(theme: Theme) {
	const dark = theme === "dark" || (theme === "system" && prefersDark());
	document.documentElement.classList.toggle("light", !dark);
	document.documentElement.classList.toggle("dark", dark);
}

interface AppState {
	token: string | null;
	theme: Theme;
	setToken: (token: string | null) => void;
	setTheme: (theme: Theme) => void;
	toggleTheme: () => void;
}

export const useApp = create<AppState>()(
	persist(
		(set, get) => ({
			token: null,
			theme: "system",
			setToken: (token) => set({ token }),
			setTheme: (theme) => {
				set({ theme });
				applyTheme(theme);
			},
			toggleTheme: () => get().setTheme(get().theme === "dark" ? "light" : "dark"),
		}),
		{ name: "waypointd-state" },
	),
);
