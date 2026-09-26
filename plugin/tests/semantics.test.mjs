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
    ? await transformTsPattern(source, {
      sourceMaps: false,
      swcOptions: options,
    })
    : await transform(source, options)
  const module = { exports: {} }
  new Function("require", "module", "exports", code)(
    require,
    module,
    module.exports,
  )
  return { code, run: module.exports.run }
}

const assertEquivalent = async (source, expected) => {
  const baseline = await evaluate(source, false)
  const compiled = await evaluate(source, true)
  assert.deepEqual(baseline.run(), expected, "raw ts-pattern reference")
  assert.deepEqual(compiled.run(), expected, compiled.code)
  return compiled.code
}

// Ordinary functions are intentional here: callback arguments and dynamic this
// are observable parts of the ts-pattern protocol, unlike arrow functions.
for (
  const { name, input, chain, declaration, expected } of [
    {
      name: "ternary",
      input: "1",
      chain: ".with(1, handler).otherwise(() => 0)",
      expected: 1,
    },
    {
      name: "primitive switch",
      input: "1",
      chain: ".with(1, handler).otherwise(() => 0)",
      declaration: "arrow",
      expected: 1,
    },
    {
      name: "object switch",
      input: "{ kind: 'a' }",
      chain:
        ".with({ kind: 'a' }, handler).with({ kind: 'b' }, () => 0).otherwise(() => 0)",
      declaration: "arrow",
      expected: { kind: "a" },
    },
    {
      name: "assignment if",
      input: "1",
      chain: ".with(P.number, handler).exhaustive()",
      expected: 1,
    },
    {
      name: "assignment object switch",
      input: "{ kind: 'a' }",
      chain:
        ".with({ kind: 'a' }, handler).with({ kind: 'b' }, () => 0).exhaustive()",
      expected: { kind: "a" },
    },
    {
      name: "anonymous selection",
      input: "{ value: 1 }",
      chain: ".with({ value: P.select() }, handler).otherwise(() => 0)",
      expected: 1,
    },
    {
      name: "named selection",
      input: "{ value: 1 }",
      chain: ".with({ value: P.select('value') }, handler).otherwise(() => 0)",
      expected: { value: 1 },
    },
  ]
) {
  test(`${name} preserves both handler arguments`, async () => {
    const match = `match(input)${chain}`
    const code = await assertEquivalent(
      `
      import { match, P } from "ts-pattern";
      export const run = () => {
        const input = ${input};
        const handler = function(selected, original) {
          return [selected, original === input, arguments.length, this === undefined];
        };
        ${
        declaration === "arrow"
          ? `const select = () => ${match}; const result = select();`
          : `const result = ${match};`
      }
        return result;
      };
    `,
      [expected, true, 2, true],
    )
    assert.doesNotMatch(
      code,
      /\.with\(/,
      "the supported chain must be compiled",
    )
  })
}

for (const terminal of ["with", "otherwise"]) {
  test(`${terminal} invokes member handlers without binding their receiver`, async () => {
    const code = await assertEquivalent(
      `
      import { match } from "ts-pattern";
      export const run = () => {
        const input = ${terminal === "with" ? 1 : 2};
        const callbacks = { handler: function(value) {
          return [value, this === undefined, arguments.length];
        }};
        return match(input)
          .with(1, ${terminal === "with" ? "callbacks.handler" : "() => 0"})
          .otherwise(${
        terminal === "otherwise" ? "callbacks.handler" : "() => 0"
      });
      };
    `,
      [terminal === "with" ? 1 : 2, true, terminal === "with" ? 2 : 1],
    )
    assert.doesNotMatch(code, /\.with\(/)
  })
}

test("P.when invokes member predicates with one argument and no receiver", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const input = 1;
      const callbacks = { predicate: function(value) {
        return value === input && this === undefined && arguments.length === 1;
      }};
      return match(input).with(P.when(callbacks.predicate), () => true).otherwise(() => false);
    };
  `,
    true,
  )
  assert.doesNotMatch(code, /\.with\(/)
})

for (const declaration of ["expression", "arrow", "assignment"]) {
  test(`${declaration} distinguishes positive and negative zero patterns`, async () => {
    const terminal = declaration === "assignment"
      ? "exhaustive()"
      : "otherwise(() => 'other')"
    const scalar =
      `match(input).with(0, () => 'positive').with(-0, () => 'negative').${terminal}`
    const object =
      `match(input).with({ value: 0 }, () => 'positive').with({ value: -0 }, () => 'negative').${terminal}`
    const body = (chain) =>
      declaration === "arrow"
        ? `input => ${chain}`
        : `input => { const result = ${chain}; return result; }`
    const code = await assertEquivalent(
      `
      import { match } from "ts-pattern";
      const scalar = ${body(scalar)};
      const object = ${body(object)};
      export const run = () => [scalar(0), scalar(-0), object({value: 0}), object({value: -0})];
    `,
      ["positive", "negative", "positive", "negative"],
    )
    assert.doesNotMatch(code, /\.with\(/)
  })
}

test("zero matching is not captured by a local Object binding", async () => {
  const code = await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      const Object = { is: () => false };
      const input = 0;
      return [match(input).with(0, () => true).otherwise(() => false), Object.is(0, 0)];
    };
  `,
    [true, false],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

for (const kind of ["const", "let"]) {
  test(`${kind} initializer retains its temporal dead zone`, async () => {
    await assertEquivalent(
      `
      import { match } from "ts-pattern";
      export const run = () => {
        try {
          ${kind} value = match(1).with(1, () => value).exhaustive();
          return value;
        } catch (error) { return error.name; }
      };
    `,
      "ReferenceError",
    )
  })
}

test("const match binding remains immutable after initialization", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      const value = match(1).with(1, () => 3).exhaustive();
      try { value = 4; } catch (error) { return [value, error.name]; }
      return value;
    };
  `,
    [3, "TypeError"],
  )
})

test("var initializer observes the previous value and remains assignable", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      var value = 2;
      var value = match(1).with(1, () => value + 1).exhaustive();
      value += 1;
      return value;
    };
  `,
    4,
  )
})

test("initializer preserves closure capture and evaluates input once", async () => {
  await assertEquivalent(
    `
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
  `,
    [3, 1],
  )
})

test("throwing match initializer does not initialize its lexical binding", async () => {
  await assertEquivalent(
    `
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
  `,
    "ReferenceError",
  )
})
