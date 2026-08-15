import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { devtools } from "@tanstack/devtools-vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const apiPort = process.env.WAYPOINTD_SERVE_PORT ?? "8080";

export default defineConfig({
	plugins: [
		devtools(),
		tanstackRouter({ target: "react", autoCodeSplitting: true }),
		react(),
		tailwindcss(),
	],
	resolve: {
		alias: {
			"#": path.resolve(__dirname, "./src"),
			"@": path.resolve(__dirname, "./src"),
		},
	},
	server: {
		port: 3000,
		proxy: {
			"/api": `http://localhost:${apiPort}`,
			"/keywords": `http://localhost:${apiPort}`,
			"/open": `http://localhost:${apiPort}`,
		},
	},
});
