// @ts-self-types="./transform.d.ts"
import { transform, transformSync } from "@swc/core"
import { statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const wasmUrl = new URL(
  "./target/wasm32-wasip1/release/swc_plugin_ts_pattern.wasm.bin",
  import.meta.url,
)

// Per-user, so other users cannot plant compiled plugin code.
const cacheDirectory = () =>
  join(
    globalThis.process?.env?.XDG_CACHE_HOME || join(homedir(), ".cache"),
    "swc-plugin-ts-pattern",
  )

const localPluginPath = () => fileURLToPath(wasmUrl)

export const defaultPluginPath = () => Promise.resolve(localPluginPath())

const cacheRootFor = (pluginPath) => {
  const { mtimeMs, size } = statSync(pluginPath)
  return join(cacheDirectory(), "swc", `${size}-${Math.trunc(mtimeMs)}`)
}

const parserFor = (filename) => ({
  syntax: "typescript",
  tsx: /\.tsx$/i.test(filename),
  decorators: false,
})

const moduleTypeFor = (filename) =>
  /\.cts$/i.test(filename) ? "commonjs" : "es6"

const buildOptions = (options = {}) => {
  const filename = options.filename ?? "input.ts"
  const pluginPath = options.pluginPath ?? localPluginPath()
  const swcOptions = options.swcOptions ?? {}
  return {
    ...swcOptions,
    filename,
    sourceMaps: options.sourceMaps ?? swcOptions.sourceMaps ?? "inline",
    jsc: {
      ...swcOptions.jsc,
      parser: swcOptions.jsc?.parser ?? parserFor(filename),
      target: swcOptions.jsc?.target ?? "es2022",
      experimental: {
        ...swcOptions.jsc?.experimental,
        cacheRoot: swcOptions.jsc?.experimental?.cacheRoot ??
          cacheRootFor(pluginPath),
        plugins: [
          [pluginPath, options.pluginOptions ?? {}],
          ...(swcOptions.jsc?.experimental?.plugins ?? []),
        ],
      },
    },
    module: swcOptions.module ?? { type: moduleTypeFor(filename) },
  }
}

export const transformTsPattern = async (source, options = {}) =>
  await transform(
    source,
    buildOptions({
      ...options,
      pluginPath: options.pluginPath ?? await defaultPluginPath(),
    }),
  )

export const transformTsPatternSync = (source, options = {}) =>
  transformSync(source, buildOptions(options))
