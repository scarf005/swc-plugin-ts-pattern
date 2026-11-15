import { readdir, readFile, stat } from "node:fs/promises"
import { assert, test } from "vite-plus/test"
import { formatValue, getModule } from "./runtime.ts"
import { pluginPath, transformWithSwcWasm } from "./swc-wasm-transform.ts"

const examplesRoot = new URL("../../examples/", import.meta.url)

const transformExample = async (source: string, options: { plugin: boolean }) =>
  getModule(
    (
      await transformWithSwcWasm({
        code: source,
        moduleType: "commonjs",
        plugin: options.plugin,
      })
    ).code,
  )

test("formatValue formats playground runtime values", () => {
  assert.deepEqual(formatValue(20n), "20n")
  assert.deepEqual(formatValue(Symbol.for("ok")), "Symbol(ok)")
  assert.deepEqual(formatValue(new Set([1, "a"])), 'Set(2) { 1, "a" }')
  assert.deepEqual(formatValue(new Map([["a", 1]])), 'Map(1) { "a" => 1 }')
})

const getExampleNames = async () => {
  const names: string[] = []
  for (const entry of await readdir(examplesRoot, { withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".ts")) names.push(entry.name)
  }
  names.sort()
  assert(names.length > 0)
  return names
}

test("all examples run in the playground runtime", async () => {
  await stat(pluginPath)

  for (const name of await getExampleNames()) {
    const source = await readFile(new URL(name, examplesRoot), "utf8")
    const [baseline, optimized] = await Promise.all([
      transformExample(source, { plugin: false }),
      transformExample(source, { plugin: true }),
    ])

    assert(baseline.inputs?.length, `${name}: missing baseline inputs`)
    assert(optimized.inputs?.length, `${name}: missing optimized inputs`)
    assert.deepEqual(
      baseline.inputs.length,
      optimized.inputs.length,
      `${name}: input length differs`,
    )

    for (const [index, baselineInput] of baseline.inputs.entries()) {
      const optimizedInput = optimized.inputs[index]
      assert.deepEqual(
        formatValue(baselineInput),
        formatValue(optimizedInput),
        `${name}: input ${index} differs`,
      )
      assert.deepEqual(
        formatValue(baseline.run(baselineInput)),
        formatValue(optimized.run(optimizedInput)),
        `${name}: output ${index} differs`,
      )
    }
  }
})

test("all match-based examples are compiled away", async () => {
  await stat(pluginPath)

  for (const name of await getExampleNames()) {
    const source = await readFile(new URL(name, examplesRoot), "utf8")
    if (!source.includes("match(")) continue

    const output = (
      await transformWithSwcWasm({
        code: source,
        moduleType: "commonjs",
        plugin: true,
      })
    ).code
      // The no-match fallback throws ts-pattern's own NonExhaustiveError.
      .replace(/\(0, [\w$]+\.match\)\([\w$]+\)\.exhaustive\(\)/g, "")

    assert(!output.includes(".with("), `${name}: still contains .with()`)
    assert(
      !output.includes(".otherwise(") && !output.includes(".exhaustive("),
      `${name}: still contains ts-pattern chain`,
    )
  }
})
