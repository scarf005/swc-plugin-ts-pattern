// @ts-self-types="./loader.d.ts"
import { readFileSync, statSync } from "node:fs"
import { dirname, extname, isAbsolute, join, resolve as resolvePath } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { transformTsPatternSync } from "./transform.mjs"

const extensions = new Set([".ts", ".tsx", ".mts", ".cts"])
// TypeScript resolves an emitted-JavaScript specifier to its source file.
const sourceExtensions = {
  "": [".ts", ".tsx", ".mts", ".cts"],
  ".js": [".ts", ".tsx"],
  ".jsx": [".tsx"],
  ".mjs": [".mts"],
  ".cjs": [".cts"],
}
let options = {}

export const initialize = (data = {}) => {
  options = data
}

const isRelativeSpecifier = (specifier) =>
  specifier.startsWith("./") || specifier.startsWith("../") || specifier.startsWith("/")

const maybeFile = (url) => {
  try {
    return statSync(fileURLToPath(url)).isFile()
  } catch {
    return false
  }
}

export const resolve = (specifier, context, nextResolve) => {
  try {
    return nextResolve(specifier, context)
  } catch (error) {
    const extension = extname(specifier)
    const candidates = sourceExtensions[extension]
    if (!isRelativeSpecifier(specifier) || !candidates) throw error

    const cwd = globalThis.process?.cwd?.() ?? globalThis.Deno?.cwd?.() ?? "."
    const base = context.parentURL?.startsWith("file:")
      ? fileURLToPath(new URL(".", context.parentURL))
      : cwd
    const path = isAbsolute(specifier) ? specifier : resolvePath(base, specifier)
    const stem = path.slice(0, path.length - extension.length)

    for (const candidate of candidates) {
      const url = pathToFileURL(`${stem}${candidate}`).href
      if (maybeFile(url)) return { url, shortCircuit: true }
    }

    throw error
  }
}

const packageType = (directory) => {
  try {
    return JSON.parse(readFileSync(join(directory, "package.json"), "utf8")).type
  } catch (error) {
    if (error?.code !== "ENOENT") throw error
    const parent = dirname(directory)
    return parent === directory ? undefined : packageType(parent)
  }
}

const formatFor = (filename) => {
  const extension = extname(filename)
  if (extension === ".mts") return "module"
  if (extension === ".cts") return "commonjs"
  // Deno always loads .ts and .tsx as ES modules.
  if (globalThis.Deno) return "module"
  return packageType(dirname(filename)) === "commonjs" ? "commonjs" : "module"
}

export const load = (url, context, nextLoad) => {
  if (!url.startsWith("file:")) return nextLoad(url, context)

  const filename = fileURLToPath(url)
  if (!extensions.has(extname(filename))) return nextLoad(url, context)

  const format = formatFor(filename)
  const source = readFileSync(filename, "utf8")
  const result = transformTsPatternSync(source, {
    ...options,
    filename,
    pluginPath: options.pluginPath,
    swcOptions: {
      ...options.swcOptions,
      module: options.swcOptions?.module ?? { type: format === "commonjs" ? "commonjs" : "es6" },
    },
  })

  return { format, shortCircuit: true, source: result.code }
}
