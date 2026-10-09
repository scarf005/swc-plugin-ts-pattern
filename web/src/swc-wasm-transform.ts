import { existsSync, readFileSync, statSync } from "node:fs"
import { createRequire } from "node:module"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"

export type ModuleType = "es6" | "commonjs"

type SwcOutput = { code: string; diagnostics?: unknown[] }
type SwcWasmBinding = {
  transform: (
    code: string,
    options: unknown,
    experimentalPluginBytesResolver?: Record<string, Uint8Array>,
  ) => Promise<SwcOutput>
}

type TransformOptions = {
  code: string
  moduleType: ModuleType
  plugin: boolean
}

export const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")
export const pluginRoot = resolve(webRoot, "../plugin")
export const pluginPath = resolve(
  pluginRoot,
  "target/wasm32-wasip1/release/swc_plugin_ts_pattern.wasm",
)

const pluginName = "@scarf005/swc-plugin-ts-pattern"
const require = createRequire(import.meta.url)
const swcWasmPackagePath = resolve(
  pluginRoot,
  "node_modules/@swc/binding_core_wasm",
)

export const hasSwcWasmBinding = () => existsSync(swcWasmPackagePath)

const loadSwcWasm = () => require(swcWasmPackagePath) as SwcWasmBinding

const cacheRootForPlugin = () => {
  const { mtimeMs, size } = statSync(pluginPath)
  return resolve(webRoot, ".swc-cache", `${size}-${Math.trunc(mtimeMs)}`)
}

export const transformWithSwcWasm = async (
  { code, moduleType, plugin }: TransformOptions,
) => {
  const plugins: [string, Record<string, never>][] = plugin
    ? [[pluginName, {}]]
    : []
  const pluginBytes = plugin
    ? { [pluginName]: readFileSync(pluginPath) }
    : undefined

  return await loadSwcWasm().transform(
    code,
    {
      filename: "input.ts",
      sourceMaps: false,
      jsc: {
        parser: { syntax: "typescript", tsx: false },
        target: "es2022",
        experimental: {
          cacheRoot: plugin ? cacheRootForPlugin() : undefined,
          plugins,
        },
      },
      module: { type: moduleType },
    },
    pluginBytes,
  )
}
