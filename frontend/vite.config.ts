import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rolldownOptions: {
      output: {
        codeSplitting: {
          // Capture React first, keeping the shared runtime below its consumers.
          // Keep recursive dependency inclusion to avoid splitting shared internals.
          groups: [
            {
              name: "react",
              test: /node_modules[\\/](?:react|react-dom|scheduler)[\\/]/,
              priority: 30,
            },
            {
              name: "aria",
              test: /node_modules[\\/](?:react-aria(?:-components)?|react-stately|@react-aria|@react-stately|@react-types|@internationalized)[\\/]/,
              priority: 20,
            },
            {
              name: "query",
              test: /node_modules[\\/]@tanstack[\\/](?:react-query|query-core)[\\/]/,
              priority: 10,
            },
          ],
        },
      },
    },
  },
  server: { proxy: { "/api": "http://127.0.0.1:8080" } },
});
