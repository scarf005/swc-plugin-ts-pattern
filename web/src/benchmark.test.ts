/// <reference lib="deno.ns" />

import { assertEquals } from "jsr:@std/assert"
import {
  benchmarkInputsEqual,
  benchmarkModules,
  benchmarkOrder,
  type BenchResult,
  formatBenchmarkComparison,
  getBenchmarkSink,
  measure,
  prepareBenchmarkInputs,
} from "./benchmark.ts"
import {
  garbageInputPairsForInputs,
  garbageValues,
  seedFromString,
} from "./property.ts"
import { formatValue } from "./runtime.ts"

for (const [baselineMs, optimizedMs] of [[0, 1], [1, 0], [0, 0], [1, Infinity]]) {
  Deno.test(`unmeasurable samples do not claim speedups (${baselineMs}, ${optimizedMs})`, () => {
    const baselineRun = () => 1
    const optimizedRun = () => 1
    const result = benchmarkModules({
      baseline: { code: "baseline", run: baselineRun, inputs: [1] },
      optimized: { code: "optimized", run: optimizedRun, inputs: [1] },
      count: 1000,
      measureFn: (run) => ({
        ms: run === baselineRun ? baselineMs : optimizedMs,
      }),
    })
    if (result.identicalCode) throw new Error("Expected distinct code")
    assertEquals(result.speedup, null)
    const text = formatBenchmarkComparison(result, {
      count: 1000,
      inputCount: 1,
      generatedInputCount: 0,
      checkedInputCount: 1,
      rejectedInputCount: 0,
      runtimeLabel: "browser benchmark",
    })
    assertEquals(/Infinity|NaN|0\.000 µs\/iteration/.test(text), false)
    assertEquals(text.includes("increase iterations/sample"), true)
  })
}

Deno.test("benchmarkOrder alternates execution order", () => {
  assertEquals(benchmarkOrder(5), [
    ["baseline", "optimized"],
    ["optimized", "baseline"],
    ["baseline", "optimized"],
    ["optimized", "baseline"],
    ["baseline", "optimized"],
  ])
})

Deno.test("garbageValues generates deterministic edge-case inputs", () => {
  const seed = seedFromString("intro-html")

  assertEquals(
    garbageValues({ seed, count: 4 }).map(formatValue),
    garbageValues({ seed, count: 4 }).map(formatValue),
  )
  assertEquals(garbageValues({ seed, count: 4 }).slice(0, 4), [
    null,
    undefined,
    true,
    false,
  ])
})

Deno.test("garbageInputPairsForInputs uses fast-check to preserve paired input shape", () => {
  assertEquals(
    garbageInputPairsForInputs([["a", "b"]], [["a", "b"]], {
      seed: 1,
      count: 4,
    }).every((pair) =>
      Array.isArray(pair.baselineInput) &&
      Array.isArray(pair.optimizedInput) &&
      pair.baselineInput.length === 2 &&
      pair.optimizedInput.length === 2
    ),
    true,
  )
})

Deno.test("benchmarkInputsEqual compares formatted values", () => {
  assertEquals(
    benchmarkInputsEqual([1n, new Set([1, 2])], [1n, new Set([1, 2])]),
    true,
  )
  assertEquals(benchmarkInputsEqual([1], [2]), false)
})

Deno.test("measure times run(input) without formatting results", () => {
  const result = {
    toString: () => {
      throw new Error("should not format")
    },
  }
  const bench = measure(() => result, [1], 3)

  assertEquals(typeof bench.ms, "number")
  assertEquals(getBenchmarkSink(), result)
})

Deno.test("prepareBenchmarkInputs validates generated garbage before benchmarking", () => {
  const baseline = {
    run: (input: unknown) => {
      if (input === "throw") throw new Error("same")
      return input === "ok" ? 1 : 0
    },
    inputs: ["ok"],
  }
  const optimized = {
    run: (input: unknown) => {
      if (input === "throw") throw new Error("same")
      return input === "ok" ? 1 : 0
    },
    inputs: ["ok"],
  }

  assertEquals(
    prepareBenchmarkInputs({
      baseline,
      optimized,
      fallbackInputs: [],
      generatedInputPairs: [
        { baselineInput: "garbage", optimizedInput: "garbage" },
        { baselineInput: "throw", optimizedInput: "throw" },
      ],
    }),
    {
      baselineInputs: ["ok", "garbage"],
      optimizedInputs: ["ok", "garbage"],
      generatedInputCount: 2,
      checkedInputCount: 3,
      rejectedInputCount: 1,
    },
  )
})

Deno.test("prepareBenchmarkInputs keeps compiled module inputs paired", () => {
  class BaselineClass {}
  class OptimizedClass {}
  const baselineInput = new BaselineClass()
  const optimizedInput = new OptimizedClass()

  assertEquals(
    prepareBenchmarkInputs({
      baseline: {
        run: (input) => input instanceof BaselineClass ? "matched" : "missed",
        inputs: [baselineInput],
      },
      optimized: {
        run: (input) => input instanceof OptimizedClass ? "matched" : "missed",
        inputs: [optimizedInput],
      },
      fallbackInputs: [],
      generatedInputPairs: [],
    }),
    {
      baselineInputs: [baselineInput],
      optimizedInputs: [optimizedInput],
      generatedInputCount: 0,
      checkedInputCount: 1,
      rejectedInputCount: 0,
    },
  )
})

Deno.test("benchmarkModules reports identical generated code", () => {
  const run = (input: unknown) => input
  const result = benchmarkModules({
    baseline: { code: "same", run, inputs: [1] },
    optimized: { code: "same", run, inputs: [1] },
    count: 10,
  })

  assertEquals(result, {
    identicalCode: true,
    note: "Generated code is identical. No SWC optimization was applied.",
  })
})

Deno.test("formatBenchmarkComparison labels samples and iterations", () => {
  assertEquals(
    formatBenchmarkComparison({
      identicalCode: false,
      baseline: { ms: 94 },
      optimized: { ms: 2 },
      speedup: 47,
      sampleCount: 7,
    }, {
      count: 100000,
      inputCount: 3,
      generatedInputCount: 64,
      checkedInputCount: 67,
      rejectedInputCount: 4,
      runtimeLabel: "browser benchmark",
    }),
    [
      "browser benchmark: 7 samples × 100000 iterations/sample",
      "inputs: 3 benchmarked, 64 generated, 67 checked, 4 rejected",
      "ts-pattern: 94.00 ms/sample (0.940 µs/iteration)",
      "compiled: 2.00 ms/sample (0.020 µs/iteration)",
      "ts-pattern SWC plugin: 47.00x faster than ts-pattern",
    ].join("\n"),
  )
})

Deno.test("benchmarkModules uses alternating samples and median", () => {
  const baselineRun = (input: unknown) => input
  const optimizedRun = (input: unknown) => input
  const calls: string[] = []
  const times = {
    baseline: [50, 30, 20, 10, 40],
    optimized: [35, 25, 15, 5, 45],
  }
  const measureFn = (run: typeof baselineRun): BenchResult => {
    const key = run === baselineRun ? "baseline" : "optimized"
    calls.push(key)
    return { ms: times[key].shift() ?? 0 }
  }

  const result = benchmarkModules({
    baseline: { code: "baseline", run: baselineRun, inputs: [1] },
    optimized: { code: "optimized", run: optimizedRun, inputs: [1] },
    count: 10,
    sampleCount: 3,
    warmupCount: 1,
    measureFn,
  })

  assertEquals(calls, [
    "baseline",
    "optimized",
    "optimized",
    "baseline",
    "baseline",
    "optimized",
    "optimized",
    "baseline",
    "baseline",
    "optimized",
  ])
  assertEquals(result, {
    identicalCode: false,
    baseline: { ms: 20 },
    optimized: { ms: 15 },
    speedup: 20 / 15,
    sampleCount: 3,
  })
})
