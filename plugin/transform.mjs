// @ts-self-types="./transform.d.ts"
import { transform, transformSync } from "@swc/core"
import { createHash, randomUUID } from "node:crypto"
import { statSync } from "node:fs"
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises"
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

let remotePluginPath
let remotePluginPromise

const localPluginPath = () => {
  if (wasmUrl.protocol === "file:") return fileURLToPath(wasmUrl)
  if (remotePluginPath) return remotePluginPath
  throw new Error(
    "Resolve the remote WASM with await defaultPluginPath() before a synchronous transform",
  )
}

const maxWasmBytes = 64 * 1024 * 1024

const readWasm = async (response) => {
  const reader = response.body?.getReader()
  if (!reader) throw new Error(`Empty plugin WASM: ${wasmUrl}`)
  const chunks = []
  let size = 0
  try {
    if (Number(response.headers.get("content-length")) > maxWasmBytes) {
      throw new Error(`Plugin WASM exceeds 64 MiB: ${wasmUrl}`)
    }
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > maxWasmBytes) {
        throw new Error(`Plugin WASM exceeds 64 MiB: ${wasmUrl}`)
      }
      chunks.push(value)
    }
  } catch (error) {
    try {
      await reader.cancel()
    } catch { /* Preserve the download failure. */ }
    throw error
  } finally {
    reader.releaseLock()
  }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return bytes
}

const isWasm = (bytes) =>
  [0, 97, 115, 109, 1, 0, 0, 0].every((byte, index) => bytes[index] === byte)

const readCachedPlugin = async (path) => {
  try {
    return isWasm(await readFile(path)) ? path : undefined
  } catch (error) {
    if (error.code === "ENOENT") return undefined
    throw error
  }
}

// A versioned remote package never changes its WASM, so a cached copy keyed
// by URL avoids a download on every start and works offline.
const downloadPlugin = async () => {
  const directory = join(cacheDirectory(), "wasm")
  const key = createHash("sha256").update(wasmUrl.href).digest("hex")
  const path = join(directory, `${key}.wasm.bin`)
  remotePluginPath = await readCachedPlugin(path)
  if (remotePluginPath) return path
  const response = await fetch(wasmUrl)
  if (!response.ok) {
    throw new Error(
      `Unable to download plugin WASM: ${response.status} ${wasmUrl}`,
    )
  }
  const bytes = await readWasm(response)
  if (!isWasm(bytes)) throw new Error(`Invalid plugin WASM: ${wasmUrl}`)
  await mkdir(directory, { recursive: true, mode: 0o700 })
  const temporary = `${path}.${randomUUID()}`
  try {
    await writeFile(temporary, bytes, { flag: "wx", mode: 0o600 })
    await rename(temporary, path)
  } finally {
    await rm(temporary, { force: true })
  }
  remotePluginPath = path
  return path
}

export const defaultPluginPath = async () => {
  if (wasmUrl.protocol === "file:" || remotePluginPath) return localPluginPath()
  remotePluginPromise ??= downloadPlugin().catch((error) => {
    remotePluginPromise = undefined
    throw error
  })
  return await remotePluginPromise
}

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
