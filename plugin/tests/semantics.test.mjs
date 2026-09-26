import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { test } from "node:test"
import { transform } from "@swc/core"
import { transformTsPattern } from "../transform.mjs"

const require = createRequire(import.meta.url)
const evaluate = async (source, plugin) => {
  const options = {
    filename: "semantics.ts",
    sourceMaps: false,
    jsc: { parser: { syntax: "typescript" }, target: "es2022" },
    module: { type: "commonjs" },
  }
  const { code } = plugin
    ? await transformTsPattern(source, { sourceMaps: false, swcOptions: options })
    : await transform(source, options)
  const module = { exports: {} }
  new Function("require", "module", "exports", code)(require, module, module.exports)
  return { code, run: module.exports.run }
}

const assertEquivalent = async (source, expected) => {
  const baseline = await evaluate(source, false)
  const compiled = await evaluate(source, true)
  assert.deepEqual(baseline.run(), expected, "raw ts-pattern reference")
  assert.deepEqual(compiled.run(), expected, compiled.code)
  return compiled.code
}

for (const kind of ["const", "let"]) {
  test(`${kind} initializer retains its temporal dead zone`, async () => {
    await assertEquivalent(`
      import { match } from "ts-pattern";
      export const run = () => {
        try {
          ${kind} value = match(1).with(1, () => value).exhaustive();
          return value;
        } catch (error) { return error.name; }
      };
    `, "ReferenceError")
  })
}

test("const match binding remains immutable after initialization", async () => {
  await assertEquivalent(`
    import { match } from "ts-pattern";
    export const run = () => {
      const value = match(1).with(1, () => 3).exhaustive();
      try { value = 4; } catch (error) { return [value, error.name]; }
      return value;
    };
  `, [3, "TypeError"])
})

test("var initializer observes the previous value and remains assignable", async () => {
  await assertEquivalent(`
    import { match } from "ts-pattern";
    export const run = () => {
      var value = 2;
      var value = match(1).with(1, () => value + 1).exhaustive();
      value += 1;
      return value;
    };
  `, 4)
})

test("initializer preserves closure capture and evaluates input once", async () => {
  await assertEquivalent(`
    import { match } from "ts-pattern";
    export const run = () => {
      let calls = 0;
      let captured;
      const value = match(++calls).with(1, () => {
        captured = () => value;
        return 3;
      }).exhaustive();
      return [captured(), calls];
    };
  `, [3, 1])
})

test("throwing match initializer does not initialize its lexical binding", async () => {
  await assertEquivalent(`
    import { match } from "ts-pattern";
    export const run = () => {
      let captured;
      try {
        const value = match(2).with(1, () => 3).with(2, () => {
          captured = () => value;
          throw new Error("handler");
        }).exhaustive();
      } catch {}
      try { return captured(); } catch (error) { return error.name; }
    };
  `, "ReferenceError")
})
