import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
	plugins: [sveltekit(), tailwindcss()],
	define: {
		__APP_VERSION__: JSON.stringify(process.env.npm_package_version || "0.0.0"),
		__GIT_COMMIT__: JSON.stringify(process.env.GIT_COMMIT || "unknown"),
		__GITHUB_URL__: JSON.stringify("https://github.com/kipsilabs/postie"),
	},
	resolve: {
		alias: {
			$lib: "./src/lib",
		},
	},
	server: {
		port: 5173,
		strictPort: true,
		proxy: {
			"/api": {
				ws: process.env.VITE_WS_PROXY === "true",
				target: "http://localhost:8080",
				changeOrigin: true,
			},
		},
	},
});
