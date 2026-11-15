import { createRequire } from "node:module"
import { transform } from "@swc/core"
import { transformTsPattern } from "../transform.mjs"

export const require = createRequire(import.meta.url)
// Every ts-pattern 5.x minor, installed as `ts-pattern-5.<minor>` aliases.
export const versions = Object.keys(require("../package.json").devDependencies).filter((name) =>
  name.startsWith("ts-pattern-"),
)

export const compile = async (source, plugin) => {
  const options = {
    filename: "semantics.ts",
    sourceMaps: false,
    jsc: { parser: { syntax: "typescript" }, target: "es2022" },
    module: { type: "commonjs" },
  }
  const { code } = plugin
    ? await transformTsPattern(source, {
        sourceMaps: false,
        swcOptions: options,
      })
    : await transform(source, options)
  return code
}

export const load = (code, version) => {
  const module = { exports: {} }
  new Function("require", "module", "exports", code)(
    (id) => require(id === "ts-pattern" ? version : id),
    module,
    module.exports,
  )
  return module.exports.run
}
