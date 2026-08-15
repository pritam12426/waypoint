import { create } from "zustand";
import { persist } from "zustand/middleware";

export type Theme = "dark" | "light";

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
			theme: "dark",
			setToken: (token) => set({ token }),
			setTheme: (theme) => {
				set({ theme });
				document.documentElement.classList.toggle("light", theme === "light");
				document.documentElement.classList.toggle("dark", theme === "dark");
			},
			toggleTheme: () => get().setTheme(get().theme === "dark" ? "light" : "dark"),
		}),
		{ name: "waypointd-state" },
	),
);
