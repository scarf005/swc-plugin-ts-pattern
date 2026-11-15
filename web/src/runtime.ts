import * as tsPattern from "ts-pattern"

export type Runnable = (input: unknown) => unknown
export type ModuleExports = { run: Runnable; inputs?: unknown[] }

const tsPatternModule = tsPattern as Record<string, unknown>

export const getModule = (code: string): ModuleExports => {
  const exports: Record<string, unknown> = {}
  const module = { exports }
  const requireShim = (name: string) => {
    if (name === "ts-pattern") return tsPatternModule
    throw new Error(`Unsupported import: ${name}`)
  }

  new Function("require", "exports", "module", code)(requireShim, exports, module)

  const loaded = module.exports as Partial<ModuleExports>
  if (typeof loaded.run !== "function") {
    throw new Error("export const run = ... is required")
  }

  return {
    run: loaded.run,
    inputs: Array.isArray(loaded.inputs) ? loaded.inputs : undefined,
  }
}

export const formatValue = (value: unknown): string =>
  formatValueInternal(value, new WeakSet<object>())

const formatValueInternal = (value: unknown, seen: WeakSet<object>): string => {
  if (value === null) return "null"
  if (value === undefined) return "undefined"

  if (typeof value === "string") return JSON.stringify(value)
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value)
  }
  if (typeof value === "bigint") return `${value}n`
  if (typeof value === "symbol") return value.toString()
  if (typeof value === "function") {
    return `[Function ${value.name || "anonymous"}]`
  }
  if (value instanceof Error) return `${value.name}: ${value.message}`
  if (value instanceof Date) {
    return Number.isNaN(value.valueOf())
      ? `Date(${JSON.stringify(String(value))})`
      : `Date(${JSON.stringify(value.toISOString())})`
  }
  if (value instanceof RegExp) return String(value)

  if (typeof value !== "object") return String(value)
  if (seen.has(value)) return "[Circular]"

  seen.add(value)
  try {
    if (Array.isArray(value)) {
      return `[${value.map((item) => formatValueInternal(item, seen)).join(", ")}]`
    }

    if (value instanceof Set) {
      return `Set(${value.size}) { ${[...value]
        .map((item) => formatValueInternal(item, seen))
        .join(", ")} }`
    }

    if (value instanceof Map) {
      return `Map(${value.size}) { ${[...value]
        .map(
          ([key, entryValue]) =>
            `${formatValueInternal(key, seen)} => ${formatValueInternal(entryValue, seen)}`,
        )
        .join(", ")} }`
    }

    const name = value.constructor?.name
    const prefix = name && name !== "Object" ? `${name} ` : ""
    const entries = Object.entries(value).map(
      ([key, entryValue]) => `${key}: ${formatValueInternal(entryValue, seen)}`,
    )
    return `${prefix}{ ${entries.join(", ")} }`
  } finally {
    seen.delete(value)
  }
}
