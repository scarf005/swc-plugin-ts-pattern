import assert from "node:assert/strict"
import { test } from "node:test"
import fc from "fast-check"
import { compile, load, require, versions } from "./harness.mjs"

// Classes for `P.instanceOf`, shared by the raw and the compiled chain.
class Base {}
class Derived extends Base {}
globalThis.DifferentialClasses = { Base, Derived }

// Each chain runs as `(input, log) => chain`. Guards push to `log`.
const chains = {
  literals:
    'match(input).with(0, () => "zero").with(NaN, () => "nan").with("a", 1n, true, null, undefined, () => "lit").otherwise(() => "other")',
  objects:
    'match(input).with({ kind: "a" }, () => 1).with({ kind: "b", value: P.number }, (v) => v.value).otherwise(() => 0)',
  optionalRecord:
    "match(input).with({ a: P.optional({ b: P.select(), c: P.number }) }, (b) => b).otherwise(() => 0)",
  nestedOptional:
    "match(input).with({ a: P.optional({ b: P.optional(P.select()) }) }, (b) => b).otherwise(() => 0)",
  optionalSelect: "match(input).with({ a: P.optional(P.select()) }, (a) => a).otherwise(() => 0)",
  namedSelections:
    'match(input).with({ a: P.select("x"), b: P.select("y") }, ({ x, y }) => [x, y]).otherwise(() => 0)',
  tuples:
    'match(input).with([P.number, P.string], ([n]) => n).with([], () => "empty").with([P._, { a: P.select() }], (a) => a).otherwise(() => 0)',
  arrayOf:
    "match(input).with(P.array({ a: P.number }), () => 1).with(P.array(P.array()), () => 2).otherwise(() => 0)",
  unionNot:
    'match(input).with(P.union("a", 1, null), (v) => v).with({ a: P.not(P.number) }, () => "not").with(P.not(P.nullish), () => "nn").otherwise(() => 0)',
  refinements:
    'match(input).with(P.string.startsWith("a").select(), (s) => s).with(P.number.between(0, 2), () => "between").with(P.number.int(), () => "int").with(P.string.startsWith("a").endsWith("b"), () => "ab").with(P.number.int().positive().lt(3), () => "small").with(P.bigint.positive().between(1n, 3n).optional().select(), (b) => b).otherwise(() => 0)',
  bigints:
    'match(input).with(P.bigint.between(0n, 0n), () => "between").with(P.bigint.gt(0n), () => "gt").with(P.bigint.lt(-1n), () => "lt").with(P.bigint.gte(1n), () => "gte").with(P.bigint.lte(-1n), () => "lte").with(P.bigint.positive(), () => "positive").with(P.bigint.negative(), () => "negative").otherwise(() => 0)',
  whenPatterns:
    'match(input).with({ a: P.when((v) => (log.push("when a"), v === 1)) }, () => 1).with({ b: P._ }, () => 2).otherwise(() => 0)',
  guards:
    'match(input).with({ a: P.number }, (v) => (log.push("guard"), v.a > 0), () => "pos").when((v) => (log.push("when"), Array.isArray(v)), () => "array").otherwise(() => 0)',
  mutatingGuard:
    'match(input).with({ a: P.when((v) => { log.push("set a"); input.a = 5; return false; }) }, () => 1).with({ a: 5 }, () => 2).otherwise(() => 0)',
  multiplePatterns:
    'match(input).with({ kind: "a", value: P.select() }, { kind: "b", value: P.select() }, (v) => v).otherwise(() => 0)',
  variadicTuples:
    'match(input).with([1, ...P.array(P.number), "a"], () => "mid").with([...P.array(P.string).select()], (s) => s).with([P.any, ...P.array(), P.any], () => "two").with([P.select(), ...P.array()], (h) => h).otherwise(() => 0)',
  arraySelections:
    'match(input).with(P.array({ a: P.select("x"), b: P.optional(P.select("y")) }), ({ x, y }) => [x, y]).with({ a: P.array(P.select()) }, { b: P.array(P.select()) }, (v) => v).with([P.number, ...P.array(P.array(P.select()))], (s) => s).with({ c: P.record(P.string, P.array(P.select())) }, (c) => c).otherwise(() => 0)',
  records:
    "match(input).with(P.record(P.string, P.number), (r) => Object.keys(r)).with(P.record(P.string, { a: P.select() }), (as) => as).otherwise(() => 0)",
  setsAndMaps:
    'match(input).with(P.set(P.number), () => "set").with(P.map(P.string, P.number), () => "map").otherwise(() => 0)',
  instanceOf:
    'match(input).with(P.instanceOf(Derived), () => "derived").with(P.instanceOf(Base), () => "base").otherwise(() => 0)',
  intersection:
    'match(input).with(P.intersection({ a: P.number }, { b: P.string }), () => 1).with({ c: P.intersection(P.any, P.when((v) => (log.push("when c"), v !== 1)), P.select()) }, (c) => c).with(P.intersection({ a: P.select("x") }, { b: P.select("y") }), (s) => s).with(P.number.and(P.select()), (n) => n).otherwise(() => 0)',
  unionSelections:
    'match(input).with({ a: P.union({ b: P.when((v) => (log.push("when b"), v !== 1)), c: P.select("x") }, { c: P.select("x") }), kind: P.select("y") }, (s) => s).with(P.union({ a: P.select() }, P.array(P.select())), (v) => v).with({ c: P.optional({ b: P.select() }).or({ a: P.select() }) }, (v) => v).otherwise(() => 0)',
  shape:
    'match(input).with(P.shape({ a: P.when((v) => (log.push("when a"), v === 1)) }).and({ b: P.select() }), (b) => b).with(P.shape({ kind: P.string }).or(P.shape([P._])), () => "shape").otherwise(() => 0)',
  exhaustive:
    'match(input).with(P.number, () => "number").with(P.string, () => "string").with({ kind: P.select() }, (k) => k).exhaustive()',
  run: 'match(input).with(P.number, () => "number").with({ a: P.select() }, (a) => a).run()',
  exhaustiveHandler:
    'match(input).returnType().with(P.string, (s) => s).with({ kind: "a" }, () => 1).exhaustive((v) => (log.push("unmatched"), typeof v))',
  argument:
    'String(match(input).with(P.number, (n) => n + 1).with({ a: 1 }, () => "a").exhaustive())',
}

const key = fc.constantFrom("kind", "a", "b", "c", "value", "0", "1")
const primitive = fc.oneof(
  fc.constantFrom(0, -0, 1, -1, 2, 0.5, NaN, Infinity, -Infinity, 1n, -1n, 0n),
  fc.constantFrom("", "a", "ab", "b", "kind", true, false, null, undefined),
  fc.integer({ min: -3, max: 5 }),
)
// A plain description; `build` makes a fresh input from it for each run.
const { value: spec } = fc.letrec((tie) => ({
  value: fc.oneof(
    { depthSize: "small", withCrossShrink: true },
    primitive,
    fc.record({ array: fc.array(tie("value"), { maxLength: 3 }), proxy: fc.boolean() }),
    fc.record({
      object: fc.array(fc.tuple(key, tie("value")), { maxLength: 4 }),
      proxy: fc.boolean(),
      volatile: fc.boolean(),
      instance: fc.constantFrom(undefined, Base, Derived),
    }),
    fc.record({ set: fc.array(primitive, { maxLength: 3 }) }),
    fc.record({
      map: fc.array(fc.tuple(fc.constantFrom("a", "b", 1), tie("value")), { maxLength: 3 }),
    }),
  ),
}))

// Logs every property access of `target`. A volatile proxy returns `"changed"`
// from the second read of a key, so an extra read can change the result.
const observed = (target, path, log, volatile) => {
  const reads = new Map()
  return new Proxy(target, {
    get: (object, name, receiver) => {
      if (!log.active) return Reflect.get(object, name, receiver)
      log.push(`get ${path}.${String(name)}`)
      const count = (reads.get(name) ?? 0) + 1
      reads.set(name, count)
      if (volatile && count > 1 && Object.hasOwn(object, name)) return "changed"
      return Reflect.get(object, name, receiver)
    },
    has: (object, name) => {
      if (log.active) log.push(`has ${path}.${String(name)}`)
      return Reflect.has(object, name)
    },
    ownKeys: (object) => {
      if (log.active) log.push(`keys ${path}`)
      return Reflect.ownKeys(object)
    },
    getOwnPropertyDescriptor: (object, name) => {
      if (log.active) log.push(`descriptor ${path}.${String(name)}`)
      return Reflect.getOwnPropertyDescriptor(object, name)
    },
  })
}

const build = (value, log, path = "input") => {
  if (value === null || typeof value !== "object") return value
  if ("array" in value) {
    const array = value.array.map((item, index) => build(item, log, `${path}[${index}]`))
    return value.proxy ? observed(array, path, log, false) : array
  }
  if ("set" in value) return new Set(value.set)
  if ("map" in value) {
    return new Map(
      value.map.map(([name, item]) => [name, build(item, log, `${path}.get(${name})`)]),
    )
  }
  const object = value.instance ? new value.instance() : {}
  for (const [name, item] of value.object) object[name] = build(item, log, `${path}.${name}`)
  return value.proxy ? observed(object, path, log, value.volatile) : object
}

const describe = (value, seen = new Set()) => {
  if (typeof value === "number") return Object.is(value, -0) ? "-0" : String(value)
  if (typeof value === "bigint") return `${value}n`
  if (typeof value === "symbol" || typeof value === "function") return String(value)
  if (value === null || typeof value !== "object") return value
  if (seen.has(value)) return "[cycle]"
  seen.add(value)
  if (value instanceof Set) return { set: [...value].map((item) => describe(item, seen)) }
  if (value instanceof Map) {
    return { map: [...value].map(([name, item]) => [describe(name, seen), describe(item, seen)]) }
  }
  if (Array.isArray(value)) return value.map((item) => describe(item, seen))
  return Object.fromEntries(
    Object.entries(value).map(([name, item]) => [name, describe(item, seen)]),
  )
}

// The result or error and every logged access of one run.
const observe = (run, value) => {
  const log = []
  log.active = true
  const input = build(value, log)
  let outcome
  try {
    outcome = { result: run(input, log) }
  } catch (error) {
    outcome = { error: `${error?.constructor?.name}: ${error?.message}` }
  }
  log.active = false
  return { ...outcome, result: describe(outcome.result), log: [...log] }
}

for (const [name, chain] of Object.entries(chains)) {
  test(`${name}: compiled chain matches raw ts-pattern on generated inputs`, async () => {
    const source = `import { match, P } from "ts-pattern";
const Base = globalThis.DifferentialClasses.Base;
const Derived = globalThis.DifferentialClasses.Derived;
export const run = (input: any, log: string[]) => ${chain};
`
    const [raw, compiled] = await Promise.all([compile(source, false), compile(source, true)])
    assert.doesNotMatch(compiled, /\.with\(/, "the chain must compile")
    // A version without one of the chain's `P` APIs fails on both sides.
    const apis = [...chain.matchAll(/P\.(\w+)/g)].map(([, api]) => api)
    for (const version of versions.filter((version) =>
      apis.every((api) => api in require(version).P),
    )) {
      const [rawRun, compiledRun] = [load(raw, version), load(compiled, version)]
      fc.assert(
        fc.property(spec, (value) => {
          assert.deepEqual(observe(compiledRun, value), observe(rawRun, value), version)
        }),
        { numRuns: 300 },
      )
    }
  })
}
