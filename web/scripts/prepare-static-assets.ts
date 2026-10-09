import { copyFileSync, mkdirSync } from "node:fs"
import { spawnSync } from "node:child_process"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const scriptRoot = dirname(fileURLToPath(import.meta.url))
const webRoot = resolve(scriptRoot, "..")
const pluginRoot = resolve(webRoot, "../plugin")
const pluginPath = resolve(
  pluginRoot,
  "target/wasm32-wasip1/release/swc_plugin_ts_pattern.wasm",
)
const publicPath = resolve(webRoot, "public/swc_plugin_ts_pattern.wasm")

const result = spawnSync("cargo", [
  "build",
  "--release",
  "--target",
  "wasm32-wasip1",
], {
  cwd: pluginRoot,
  stdio: "inherit",
})
if (result.status !== 0) {
  throw new Error("Failed to build ts-pattern SWC plugin wasm")
}

mkdirSync(dirname(publicPath), { recursive: true })
copyFileSync(pluginPath, publicPath)
