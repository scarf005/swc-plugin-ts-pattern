import * as fc from "fast-check"

export const seedFromString = (value: string) => {
  let seed = 2166136261
  for (let index = 0; index < value.length; index += 1) {
    seed = Math.imul(seed ^ value.charCodeAt(index), 16777619)
  }
  return seed >>> 0
}

const edgeCases = [
  null,
  undefined,
  true,
  false,
  0,
  Number.NaN,
  Number.POSITIVE_INFINITY,
  "",
  "__garbage__",
  [],
  {},
  { type: "__garbage__" },
  { type: "ok", data: null },
  { type: "error", error: "not an Error" },
] as const

const genericGarbageArbitrary = fc.oneof(
  fc.constantFrom<unknown>(...edgeCases),
  fc.anything({
    maxDepth: 3,
    withBigInt: true,
    withDate: true,
    withMap: true,
    withSet: true,
  }),
)

const discriminatorKeys = new Set(["type", "kind", "tag"])

const repeatArbitrary = <T>(count: number, arbitrary: fc.Arbitrary<T>) =>
  Array.from({ length: count }, () => arbitrary)

const maybeGarbage = (value: unknown, key?: string): fc.Arbitrary<unknown> =>
  discriminatorKeys.has(key ?? "")
    ? fc.oneof(...repeatArbitrary(8, fc.constant(value)), genericGarbageArbitrary)
    : fc.oneof(fc.constant(value), ...repeatArbitrary(3, genericGarbageArbitrary))

const arbitraryLike = (
  value: unknown,
  options: { depth: number; key?: string },
): fc.Arbitrary<unknown> => {
  if (options.depth >= 3) return maybeGarbage(value, options.key)
  if (Array.isArray(value)) {
    return fc.oneof(
      fc.constant(value),
      fc.tuple(...value.map((item) => arbitraryLike(item, { depth: options.depth + 1 }))),
    )
  }
  if (
    value &&
    typeof value === "object" &&
    !(value instanceof Error) &&
    !(value instanceof Date) &&
    !(value instanceof RegExp) &&
    !(value instanceof Set) &&
    !(value instanceof Map) &&
    value.constructor === Object
  ) {
    const model = Object.fromEntries(
      Object.entries(value).map(([key, entryValue]) => [
        key,
        arbitraryLike(entryValue, { depth: options.depth + 1, key }),
      ]),
    )
    return fc.oneof(fc.constant(value), fc.record(model))
  }
  return maybeGarbage(value, options.key)
}

export const garbageValues = (options: { seed: number; count: number }) => [
  ...edgeCases,
  ...fc.sample(genericGarbageArbitrary, {
    seed: options.seed,
    numRuns: options.count,
  }),
]

const sampleLike = (value: unknown, options: { seed: number; count: number }) =>
  fc.sample(arbitraryLike(value, { depth: 0 }), {
    seed: options.seed,
    numRuns: options.count,
  })

export const garbageInputPairsForInputs = (
  baselineInputs: unknown[],
  optimizedInputs: unknown[],
  options: { seed: number; count: number },
) => {
  const length = Math.min(baselineInputs.length, optimizedInputs.length)
  if (length === 0) {
    return garbageValues(options).map((input) => ({
      baselineInput: input,
      optimizedInput: input,
    }))
  }

  const indexes = fc.sample(fc.integer({ min: 0, max: length - 1 }), {
    seed: options.seed,
    numRuns: options.count,
  })

  return indexes.map((index, runIndex) => ({
    baselineInput: sampleLike(baselineInputs[index], {
      seed: options.seed + runIndex + 1,
      count: 1,
    })[0],
    optimizedInput: sampleLike(optimizedInputs[index], {
      seed: options.seed + runIndex + 1,
      count: 1,
    })[0],
  }))
}
