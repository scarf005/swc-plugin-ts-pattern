// @ts-self-types="./loader.d.ts"
import { readFileSync, statSync } from "node:fs"
import { extname, isAbsolute, resolve as resolvePath } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { transformTsPatternSync } from "./transform.mjs"

const extensions = new Set([".ts", ".tsx", ".mts", ".cts"])
const extensionOrder = [".ts", ".tsx", ".mts", ".cts"]
let options = {}

export const initialize = (data = {}) => {
  options = data
}

const isRelativeSpecifier = (specifier) =>
  specifier.startsWith("./") || specifier.startsWith("../") ||
  specifier.startsWith("/")

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
    if (!isRelativeSpecifier(specifier) || extname(specifier)) throw error

    const cwd = globalThis.process?.cwd?.() ?? globalThis.Deno?.cwd?.() ?? "."
    const base = context.parentURL?.startsWith("file:")
      ? fileURLToPath(new URL(".", context.parentURL))
      : cwd
    const withoutExtension = isAbsolute(specifier)
      ? specifier
      : resolvePath(base, specifier)

    for (const extension of extensionOrder) {
      const url = pathToFileURL(`${withoutExtension}${extension}`).href
      if (maybeFile(url)) return { url, shortCircuit: true }
    }

    throw error
  }
}

export const load = (url, context, nextLoad) => {
  if (!url.startsWith("file:")) return nextLoad(url, context)

  const filename = fileURLToPath(url)
  if (!extensions.has(extname(filename))) return nextLoad(url, context)

  const source = readFileSync(filename, "utf8")
  const result = transformTsPatternSync(source, {
    ...options,
    filename,
    pluginPath: options.pluginPath,
  })

  return {
    format: extname(filename) === ".cts" ? "commonjs" : "module",
    shortCircuit: true,
    source: result.code,
  }
}
