// @ts-self-types="./transform.d.ts"
import { transform, transformSync } from "@swc/core"
import { createHash, randomUUID } from "node:crypto"
import { statSync } from "node:fs"
import { mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const wasmUrl = new URL(
  "./target/wasm32-wasip1/release/ts_pattern_swc_plugin.wasm.bin",
  import.meta.url,
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

const downloadPlugin = async () => {
  const response = await fetch(wasmUrl)
  if (!response.ok) {
    throw new Error(
      `Unable to download plugin WASM: ${response.status} ${wasmUrl}`,
    )
  }
  const bytes = await readWasm(response)
  const header = [0, 97, 115, 109, 1, 0, 0, 0]
  if (!header.every((byte, index) => bytes[index] === byte)) {
    throw new Error(`Invalid plugin WASM: ${wasmUrl}`)
  }
  const digest = createHash("sha256").update(bytes).digest("hex")
  const directory = join(tmpdir(), "ts-pattern-swc-plugin", "wasm")
  const path = join(directory, `${digest}.wasm.bin`)
  await mkdir(directory, { recursive: true, mode: 0o700 })
  let validCache = false
  try {
    if ((await stat(path)).size === bytes.length) {
      const cached = await readFile(path)
      validCache = createHash("sha256").update(cached).digest("hex") === digest
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw error
  }
  if (!validCache) {
    const temporary = `${path}.${randomUUID()}`
    try {
      await writeFile(temporary, bytes, { flag: "wx", mode: 0o600 })
      await rename(temporary, path)
    } finally {
      await rm(temporary, { force: true })
    }
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
  return join(
    tmpdir(),
    "ts-pattern-swc-plugin",
    `${size}-${Math.trunc(mtimeMs)}`,
  )
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
