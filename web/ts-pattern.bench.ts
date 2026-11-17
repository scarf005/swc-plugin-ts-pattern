import { readdir, readFile, stat, writeFile } from "node:fs/promises"
import { afterAll, test } from "vite-plus/test"
import { prepareBenchmarkInputs } from "./src/benchmark.ts"
import { garbageInputPairsForInputs, seedFromString } from "./src/property.ts"
import { getModule, type ModuleExports } from "./src/runtime.ts"
import { pluginPath, transformWithSwcWasm } from "./src/swc-wasm-transform.ts"

type BenchTarget = {
  label: string
  module: Required<ModuleExports>
}

type BenchCase = {
  id: string
  targets: BenchTarget[]
}

const examplesRoot = new URL("../examples/", import.meta.url)
const tsPatternBenchmarkRoot = new URL("../vendor/ts-pattern-benchmark/tests/", import.meta.url)

const digitInputs = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]

const digitMatcherSource = (inputs: number[]) => `
import { match } from "ts-pattern"

type Digit = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9

export const inputs = ${JSON.stringify(inputs)} as Digit[]

export const run = (digit: Digit): string =>
  match(digit)
    .with(0, () => "zero")
    .with(1, () => "one")
    .with(2, () => "two")
    .with(3, () => "three")
    .with(4, () => "four")
    .with(5, () => "five")
    .with(6, () => "six")
    .with(7, () => "seven")
    .with(8, () => "eight")
    .with(9, () => "nine")
    .otherwise(() => "")
`

const digitNativeSource = (inputs: number[]) => `
type Digit = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9

export const inputs = ${JSON.stringify(inputs)} as Digit[]

export const run = (digit: Digit): string =>
  digit === 0 ? "zero"
    : digit === 1 ? "one"
    : digit === 2 ? "two"
    : digit === 3 ? "three"
    : digit === 4 ? "four"
    : digit === 5 ? "five"
    : digit === 6 ? "six"
    : digit === 7 ? "seven"
    : digit === 8 ? "eight"
    : digit === 9 ? "nine"
    : ""
`

const nestedMatcherSource = `
import { match, P } from "ts-pattern"

export const inputs = [
  { type: "a", value: { x: 1, y: 2 } },
  { type: "b", value: [1, 2, 3, 4] },
  { type: "c", age: 5, name: "acdfl" },
  { type: "z" },
]

export const run = (input: unknown): string =>
  match(input)
    .with({ type: "a", value: { x: P.number, y: P.number } }, () => "1")
    .with({ type: "b", value: P.array(P.number) }, () => "2")
    .with({ type: "c", name: P.string, age: P.number }, () => "3")
    .otherwise(() => "4")
`

const nestedNativeSource = `
export const inputs = [
  { type: "a", value: { x: 1, y: 2 } },
  { type: "b", value: [1, 2, 3, 4] },
  { type: "c", age: 5, name: "acdfl" },
  { type: "z" },
]

export const run = (input: unknown): string =>
  input !== null && typeof input === "object" && "type" in input && input.type === "a" &&
      "value" in input && input.value !== null && typeof input.value === "object" &&
      "x" in input.value && typeof input.value.x === "number" &&
      "y" in input.value && typeof input.value.y === "number"
    ? "1"
    : input !== null && typeof input === "object" && "type" in input && input.type === "b" &&
        "value" in input && Array.isArray(input.value) &&
        input.value.every((value) => typeof value === "number")
    ? "2"
    : input !== null && typeof input === "object" && "type" in input && input.type === "c" &&
        "name" in input && typeof input.name === "string" &&
        "age" in input && typeof input.age === "number"
    ? "3"
    : "4"
`

const benchmarkSources = [
  {
    id: "ts-pattern-benchmark/always-last-digit",
    sourceFile: "always-last-digit.mts",
    matcher: digitMatcherSource([9]),
    native: digitNativeSource([9]),
  },
  {
    id: "ts-pattern-benchmark/random-digit",
    sourceFile: "random-digit.mts",
    matcher: digitMatcherSource(digitInputs),
    native: digitNativeSource(digitInputs),
  },
  {
    id: "ts-pattern-benchmark/nested-objects",
    sourceFile: "nested-objects.mts",
    matcher: nestedMatcherSource,
    native: nestedNativeSource,
  },
]

const getExampleNames = async () => {
  const names: string[] = []
  for (const entry of await readdir(examplesRoot, { withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".ts")) names.push(entry.name)
  }
  names.sort()
  if (names.length === 0) throw new Error("No examples found")
  return names
}

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

const loadExample = async (name: string): Promise<BenchCase> => {
  const source = await readFile(new URL(name, examplesRoot), "utf8")
  const [baseline, optimized] = await Promise.all([
    transformExample(source, { plugin: false }),
    transformExample(source, { plugin: true }),
  ])

  if (!baseline.inputs?.length) throw new Error(`${name}: missing inputs`)
  if (!optimized.inputs?.length) {
    throw new Error(`${name}: missing optimized inputs`)
  }

  const inputs = prepareBenchmarkInputs({
    baseline,
    optimized,
    fallbackInputs: baseline.inputs,
    generatedInputPairs: garbageInputPairsForInputs(baseline.inputs, optimized.inputs, {
      seed: seedFromString(name),
      count: 64,
    }),
  })

  return {
    id: name.replace(/\.ts$/, ""),
    targets: [
      {
        label: "ts-pattern raw",
        module: { run: baseline.run, inputs: inputs.baselineInputs },
      },
      {
        label: "ts-pattern swc plugin",
        module: { run: optimized.run, inputs: inputs.optimizedInputs },
      },
    ],
  }
}

const assertSameOutputs = (id: string, targets: BenchTarget[]) => {
  const [first, ...rest] = targets
  if (!first) throw new Error(`${id}: missing benchmark targets`)

  for (const input of first.module.inputs) {
    const expected = first.module.run(input)
    for (const target of rest) {
      const actual = target.module.run(input)
      if (!Object.is(actual, expected)) {
        throw new Error(
          `${id}: ${target.label} returned ${String(actual)} instead of ${String(expected)}`,
        )
      }
    }
  }
}

const loadTsPatternBenchmarkCase = async (
  source: (typeof benchmarkSources)[number],
): Promise<BenchCase> => {
  await stat(new URL(source.sourceFile, tsPatternBenchmarkRoot))
  const [raw, native, optimized] = await Promise.all([
    transformExample(source.matcher, { plugin: false }),
    transformExample(source.native, { plugin: false }),
    transformExample(source.matcher, { plugin: true }),
  ])

  if (!raw.inputs?.length) throw new Error(`${source.id}: missing raw inputs`)
  if (!native.inputs?.length) {
    throw new Error(`${source.id}: missing native inputs`)
  }
  if (!optimized.inputs?.length) {
    throw new Error(`${source.id}: missing optimized inputs`)
  }

  const targets = [
    { label: "ts-pattern raw", module: raw as Required<ModuleExports> },
    {
      label: "native code",
      module: native as Required<ModuleExports>,
    },
    {
      label: "ts-pattern swc plugin",
      module: optimized as Required<ModuleExports>,
    },
  ]
  assertSameOutputs(source.id, targets)
  return { id: source.id, targets }
}

const createRunner = (module: Required<ModuleExports>) => {
  let index = 0
  return () => {
    const input = module.inputs[index]
    index = (index + 1) % module.inputs.length
    return module.run(input)
  }
}

await stat(pluginPath)
const [examples, tsPatternBenchmarkCases] = await Promise.all([
  Promise.all((await getExampleNames()).map(loadExample)),
  Promise.all(benchmarkSources.map(loadTsPatternBenchmarkCase)),
])

// Operations per second for each case and target, written to `BENCH_JSON`.
const throughput: Record<string, Record<string, number>> = {}

for (const { id, targets } of [...tsPatternBenchmarkCases, ...examples]) {
  test(id, async ({ bench }) => {
    const results = await bench.compare(
      ...targets.map(({ label, module }) => bench(label, createRunner(module))),
    )
    throughput[id] = Object.fromEntries(
      targets.map(({ label }) => [label, results.get(label).throughput.mean]),
    )
  })
}

afterAll(async () => {
  if (process.env.BENCH_JSON) {
    await writeFile(process.env.BENCH_JSON, JSON.stringify(throughput, null, 2))
  }
})
