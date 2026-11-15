/// <reference lib="deno.ns" />

import { assert, assertEquals } from "jsr:@std/assert"
import { formatValue, getModule } from "./runtime.ts"
import { pluginPath, transformWithSwcWasm } from "./swc-wasm-transform.ts"

const examplesRoot = new URL("../../examples/", import.meta.url)

const transformExample = async (
  source: string,
  options: { plugin: boolean },
) =>
  getModule(
    (await transformWithSwcWasm({
      code: source,
      moduleType: "commonjs",
      plugin: options.plugin,
    })).code,
  )

Deno.test("formatValue formats playground runtime values", () => {
  assertEquals(formatValue(20n), "20n")
  assertEquals(formatValue(Symbol.for("ok")), "Symbol(ok)")
  assertEquals(formatValue(new Set([1, "a"])), 'Set(2) { 1, "a" }')
  assertEquals(
    formatValue(new Map([["a", 1]])),
    'Map(1) { "a" => 1 }',
  )
})

const getExampleNames = async () => {
  const names: string[] = []
  for await (const entry of Deno.readDir(examplesRoot)) {
    if (entry.isFile && entry.name.endsWith(".ts")) names.push(entry.name)
  }
  names.sort()
  assert(names.length > 0)
  return names
}

Deno.test("all examples run in the playground runtime", async () => {
  await Deno.stat(pluginPath)

  for (const name of await getExampleNames()) {
    const source = await Deno.readTextFile(new URL(name, examplesRoot))
    const [baseline, optimized] = await Promise.all([
      transformExample(source, { plugin: false }),
      transformExample(source, { plugin: true }),
    ])

    assert(baseline.inputs?.length, `${name}: missing baseline inputs`)
    assert(optimized.inputs?.length, `${name}: missing optimized inputs`)
    assertEquals(
      baseline.inputs.length,
      optimized.inputs.length,
      `${name}: input length differs`,
    )

    for (const [index, baselineInput] of baseline.inputs.entries()) {
      const optimizedInput = optimized.inputs[index]
      assertEquals(
        formatValue(baselineInput),
        formatValue(optimizedInput),
        `${name}: input ${index} differs`,
      )
      assertEquals(
        formatValue(baseline.run(baselineInput)),
        formatValue(optimized.run(optimizedInput)),
        `${name}: output ${index} differs`,
      )
    }
  }
})

Deno.test("all match-based examples are compiled away", async () => {
  await Deno.stat(pluginPath)

  for (const name of await getExampleNames()) {
    const source = await Deno.readTextFile(new URL(name, examplesRoot))
    if (!source.includes("match(")) continue

    const output = (await transformWithSwcWasm({
      code: source,
      moduleType: "commonjs",
      plugin: true,
    })).code
      // The no-match fallback throws ts-pattern's own NonExhaustiveError.
      .replace(/throw \(0, [\w$]+\.match\)\([\w$]+\)\.exhaustive\(\);/g, "")

    assert(!output.includes(".with("), `${name}: still contains .with()`)
    assert(
      !output.includes(".otherwise(") && !output.includes(".exhaustive("),
      `${name}: still contains ts-pattern chain`,
    )
  }
})
