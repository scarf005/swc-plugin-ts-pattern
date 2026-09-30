import { formatValue, type ModuleExports, type Runnable } from "./runtime.ts"

export type BenchResult = { ms: number }
export type BenchmarkModule = { code: string; run: Runnable; inputs: unknown[] }
export type BenchmarkComparison =
  | {
    identicalCode: true
    note: string
  }
  | {
    identicalCode: false
    baseline: BenchResult
    optimized: BenchResult
    speedup: number | null
    sampleCount: number
  }

export type BenchmarkFormatOptions = {
  count: number
  inputCount: number
  generatedInputCount: number
  checkedInputCount: number
  rejectedInputCount: number
  runtimeLabel: string
}

export type BenchmarkInputPair = {
  baselineInput: unknown
  optimizedInput: unknown
}

export type BenchmarkInputs = {
  baselineInputs: unknown[]
  optimizedInputs: unknown[]
  generatedInputCount: number
  checkedInputCount: number
  rejectedInputCount: number
}

type BenchmarkOrder = "baseline" | "optimized"
type MeasureFn = (
  run: Runnable,
  inputs: unknown[],
  count: number,
) => BenchResult

let sink: unknown

export const measure = (
  run: Runnable,
  inputs: unknown[],
  count: number,
): BenchResult => {
  let result: unknown
  const start = performance.now()
  for (let index = 0; index < count; index += 1) {
    result = run(inputs[index % inputs.length])
  }
  const ms = performance.now() - start
  sink = result
  return { ms }
}

export const benchmarkOrder = (sampleCount: number): BenchmarkOrder[][] =>
  Array.from(
    { length: sampleCount },
    (_, index) =>
      index % 2 === 0 ? ["baseline", "optimized"] : ["optimized", "baseline"],
  )

const median = (results: BenchResult[]): BenchResult =>
  [...results].sort((left, right) => left.ms - right.ms)[
    Math.floor(results.length / 2)
  ]

type RunOutcome =
  | { ok: true; value: unknown }
  | { ok: false; error: unknown }

const runOutcome = (run: Runnable, input: unknown): RunOutcome => {
  try {
    return { ok: true, value: run(input) }
  } catch (error) {
    return { ok: false, error }
  }
}

const outcomesEqual = (left: RunOutcome, right: RunOutcome) =>
  left.ok === right.ok &&
  (left.ok && right.ok
    ? formatValue(left.value) === formatValue(right.value)
    : true)

export const prepareBenchmarkInputs = (
  options: {
    baseline: ModuleExports
    optimized: ModuleExports
    fallbackInputs: unknown[]
    generatedInputPairs: BenchmarkInputPair[]
  },
): BenchmarkInputs => {
  const baselineInputs = options.baseline.inputs?.length
    ? options.baseline.inputs
    : options.fallbackInputs
  const optimizedInputs = options.optimized.inputs?.length
    ? options.optimized.inputs
    : options.fallbackInputs

  if (baselineInputs.length !== optimizedInputs.length) {
    throw new Error("Benchmark input length differs")
  }

  const candidates = [
    ...baselineInputs.map((input, index) => ({
      baselineInput: input,
      optimizedInput: optimizedInputs[index],
    })),
    ...options.generatedInputPairs,
  ]
  const preparedBaselineInputs: unknown[] = []
  const preparedOptimizedInputs: unknown[] = []
  let rejectedInputCount = 0

  for (const [index, input] of candidates.entries()) {
    const baselineOutcome = runOutcome(
      options.baseline.run,
      input.baselineInput,
    )
    const optimizedOutcome = runOutcome(
      options.optimized.run,
      input.optimizedInput,
    )
    if (!outcomesEqual(baselineOutcome, optimizedOutcome)) {
      throw new Error(`Benchmark output ${index} differs`)
    }
    if (baselineOutcome.ok) {
      preparedBaselineInputs.push(input.baselineInput)
      preparedOptimizedInputs.push(input.optimizedInput)
    } else {
      rejectedInputCount += 1
    }
  }

  if (preparedBaselineInputs.length === 0) {
    throw new Error("No benchmarkable inputs")
  }

  return {
    baselineInputs: preparedBaselineInputs,
    optimizedInputs: preparedOptimizedInputs,
    generatedInputCount: options.generatedInputPairs.length,
    checkedInputCount: candidates.length,
    rejectedInputCount,
  }
}

export const benchmarkModules = (
  options: {
    baseline: BenchmarkModule
    optimized: BenchmarkModule
    count: number
    sampleCount?: number
    warmupCount?: number
    measureFn?: MeasureFn
  },
): BenchmarkComparison => {
  const measureFn = options.measureFn ?? measure
  const sampleCount = Math.max(3, options.sampleCount ?? 7)
  const warmupCount = Math.max(
    1,
    options.warmupCount ??
      Math.min(10000, Math.max(1000, Math.floor(options.count / 10))),
  )

  if (options.baseline.code === options.optimized.code) {
    return {
      identicalCode: true,
      note: "Generated code is identical. No SWC optimization was applied.",
    }
  }

  const baselineResults: BenchResult[] = []
  const optimizedResults: BenchResult[] = []

  for (const order of benchmarkOrder(2)) {
    for (const target of order) {
      const module = target === "baseline"
        ? options.baseline
        : options.optimized
      measureFn(module.run, module.inputs, warmupCount)
    }
  }

  for (const order of benchmarkOrder(sampleCount)) {
    for (const target of order) {
      const module = target === "baseline"
        ? options.baseline
        : options.optimized
      const result = measureFn(module.run, module.inputs, options.count)
      if (target === "baseline") baselineResults.push(result)
      else optimizedResults.push(result)
    }
  }

  const baseline = median(baselineResults)
  const optimized = median(optimizedResults)
  const speedup = baseline.ms / optimized.ms

  return {
    identicalCode: false,
    baseline,
    optimized,
    speedup: Number.isFinite(baseline.ms) && Number.isFinite(optimized.ms) &&
        baseline.ms > 0 && optimized.ms > 0 && Number.isFinite(speedup) &&
        speedup > 0
      ? speedup
      : null,
    sampleCount,
  }
}

const formatMs = (value: number) => value.toFixed(2)

const formatUs = (ms: number, count: number) => ((ms * 1000) / count).toFixed(3)

const formatMeasurement = ({ ms }: BenchResult, count: number) =>
  Number.isFinite(ms) && ms > 0
    ? `${formatMs(ms)} ms/sample (${formatUs(ms, count)} µs/iteration)`
    : "not measurable at current timer resolution"

export const formatBenchmarkComparison = (
  result: BenchmarkComparison,
  options: BenchmarkFormatOptions,
): string =>
  result.identicalCode ? result.note : [
    `${options.runtimeLabel}: ${result.sampleCount} samples × ${options.count} iterations/sample`,
    `inputs: ${options.inputCount} benchmarked, ${options.generatedInputCount} generated, ${options.checkedInputCount} checked, ${options.rejectedInputCount} rejected`,
    `ts-pattern: ${formatMeasurement(result.baseline, options.count)}`,
    `compiled: ${formatMeasurement(result.optimized, options.count)}`,
    result.speedup === null
      ? "Speedup unavailable: increase iterations/sample."
      : `ts-pattern SWC plugin: ${
        result.speedup.toFixed(2)
      }x faster than ts-pattern`,
  ].join("\n")

export const getBenchmarkSink = () => sink

export const benchmarkInputsEqual = (
  baselineInputs: unknown[],
  optimizedInputs: unknown[],
): boolean =>
  baselineInputs.length === optimizedInputs.length &&
  baselineInputs.every((input, index) =>
    formatValue(input) === formatValue(optimizedInputs[index])
  )
