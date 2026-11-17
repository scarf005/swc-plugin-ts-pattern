import preact from "@preact/preset-vite"
import { defineConfig } from "vite-plus"

export default defineConfig({
  // GitHub Pages serves the playground from /swc-plugin-ts-pattern/.
  base: process.env.BASE_PATH ?? "/",
  plugins: [preact()],
  test: {
    projects: [
      {
        test: {
          name: "unit",
          include: ["src/*.test.ts"],
          exclude: ["src/browser.test.ts"],
          testTimeout: 120_000,
        },
      },
      {
        test: {
          name: "browser",
          include: ["src/browser.test.ts"],
          // Keep import() in page callbacks; the module runner rewrites it.
          experimental: { viteModuleRunner: false },
          testTimeout: 600_000,
        },
      },
    ],
  },
})
