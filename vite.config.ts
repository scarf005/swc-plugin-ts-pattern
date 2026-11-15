import { defineConfig } from "vite-plus"

export default defineConfig({
  fmt: {
    semi: false,
    ignorePatterns: [
      "vendor/**",
      "demo/**",
      "web/src/vendor/**",
      "docs/**",
      "**/*.snap",
      "**/Cargo.toml",
    ],
  },
  lint: {
    ignorePatterns: ["vendor/**", "demo/**", "web/src/vendor/**"],
  },
})
