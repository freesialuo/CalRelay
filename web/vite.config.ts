import { defineConfig } from "vite";
export default defineConfig({
  base: "/admin/",
  // No application env variables are exposed to browser code.
  envPrefix: [],
  build: { sourcemap: false },
});
