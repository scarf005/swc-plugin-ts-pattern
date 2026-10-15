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

test("snapshots the match input before a predicate can mutate its binding", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let input = 1;
      return match(input)
        .with(P.when(() => { input = 2; return false; }), () => 0)
        .otherwise((value) => value);
    };
  `,
    1,
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("reads wildcard properties once in pattern order", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const reads = [];
      const input = {
        get first() { reads.push("first"); return 1; },
        get second() { reads.push("second"); return 2; },
      };
      const result = match(input)
        .with({ first: P._, second: P._ }, () => "matched")
        .otherwise(() => "other");
      return [result, reads];
    };
  `,
    ["matched", ["first", "second"]],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("captures selected getter values instead of rereading them", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let reads = 0;
      const input = { get value() { return ++reads; } };
      const result = match(input)
        .with({ value: P.select(P.number) }, (value) => value)
        .otherwise(() => 0);
      return [result, reads];
    };
  `,
    [1, 1],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("evaluates computed pattern keys once", async () => {
  const code = await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      let calls = 0;
      const key = () => { calls++; return "value"; };
      const input = { value: 1 };
      const result = match(input)
        .with({ [key()]: 1 }, () => "matched")
        .otherwise(() => "other");
      return [result, calls];
    };
  `,
    ["matched", 1],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("evaluates later arm arguments after an earlier match", async () => {
  const code = await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      let calls = 0;
      const next = () => { calls++; return () => 2; };
      const result = match(1)
        .with(1, () => 1)
        .with(2, next())
        .otherwise(() => 0);
      return [result, calls];
    };
  `,
    [1, 1],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("does not replay effectful const pattern initialization", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    let calls = 0;
    const key = () => { calls++; return "value"; };
    const pattern = { [key()]: 1 };
    export const run = () => {
      const result = match({ value: 1 })
        .with(pattern, () => "matched")
        .otherwise(() => "other");
      return [result, calls];
    };
  `,
    ["matched", 1],
  )
})

test("does not resolve a const pattern after object mutation", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    const pattern = { value: 1 };
    pattern.value = 2;
    const input = { value: 2 };
    export const run = () =>
      match(input).with(pattern, () => "matched").otherwise(() => "other");
  `,
    "matched",
  )
})

test("generated globals ignore lexical shadows", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const inputArray = [1];
      const inputMap = new globalThis.Map([["key", 1]]);
      const inputSet = new globalThis.Set([1]);
      const recordInput = { 1: "value" };
      const Number = Object.assign(() => 0, {
        isFinite: () => false,
        isInteger: () => false,
        isNaN: () => false,
      });
      const Array = { isArray: () => false, from: () => [] };
      const Map = class {};
      const Set = class {};
      const Reflect = { ownKeys: () => [] };
      const undefined = 1;
      return [
        match(1).with(P.number.finite(), () => true).otherwise(() => false),
        match(1).with(P.number.int(), () => true).otherwise(() => false),
        match(inputArray).with(P.array(), () => true).otherwise(() => false),
        match(inputMap).with(P.map(), () => true).otherwise(() => false),
        match(inputSet).with(P.set(), () => true).otherwise(() => false),
        match(recordInput)
          .with(P.record(P.number.gt(0), P.string), () => true)
          .otherwise(() => false),
        match(recordInput)
          .with(P.record(P.string, P.string), () => true)
          .otherwise(() => false),
        match(1).with(P.nullish, () => true).otherwise(() => false),
      ];
    };
  `,
    [true, true, true, true, true, true, true, false],
  )
})

test("record matching preserves getter reads and key/value evaluation", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const eagerReads = [];
      const eagerInput = {
        get first() { eagerReads.push("first"); return "bad"; },
        get second() { eagerReads.push("second"); return 2; },
      };
      const eagerResult = match(eagerInput)
        .with(P.record(P.string, P.number), () => true)
        .otherwise(() => false);

      const shortReads = [];
      const shortInput = {
        get first() { shortReads.push("first"); return 1; },
      };
      const shortResult = match(shortInput)
        .with(P.record(P.number, P.number), () => true)
        .otherwise(() => false);

      let selectedReads = 0;
      const selectedInput = { get value() { return ++selectedReads; } };
      const selectedResult = match(selectedInput)
        .with(P.record(P.string, P.number.select()), (values) => values.join(","))
        .otherwise(() => "");

      const staticInput = { value: 1 };
      const staticResult = match(staticInput)
        .with(P.record(P.string, P.number.select()), (values) => values.join(","))
        .otherwise(() => "");

      let arrowReads = 0;
      const arrow = (value) => match(value)
        .with(P.record(P.string, P.number.select()), (values) => values.join(","))
        .otherwise(() => "");
      const arrowResult = arrow({ get value() { return ++arrowReads; } });
      return [
        [eagerResult, eagerReads],
        [shortResult, shortReads],
        [selectedResult, selectedReads],
        [staticResult, 1],
        [arrowResult, arrowReads],
      ];
    };
  `,
    [[false, ["first"]], [false, ["first"]], ["1", 1], ["1", 1], ["1", 1]],
  )
})

test("record value getters run before effectful key checks", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const plainRegex = /a/y;
      const plainEvents = [];
      const plainInput = {
        get a() { plainEvents.push(plainRegex.lastIndex); return plainRegex.lastIndex; },
      };
      const plain = match(plainInput)
        .with(P.record(P.string.regex(plainRegex), 0), () => true)
        .otherwise(() => false);
      const selectedRegex = /a/y;
      const selectedEvents = [];
      const selectedInput = {
        get a() { selectedEvents.push(selectedRegex.lastIndex); return selectedRegex.lastIndex; },
      };
      const selected = match(selectedInput)
        .with(P.record(P.string.regex(selectedRegex), P.number.select()), (values) => values)
        .otherwise(() => []);
      const valueRegex = /a/y;
      const keyMismatch = match({ a: "a" })
        .with(P.record(P.number, P.string.regex(valueRegex).select()), () => true)
        .otherwise(() => false);
      return [[plain, plainEvents], [selected, selectedEvents], [keyMismatch, valueRegex.lastIndex]];
    };
  `,
    [[true, [0]], [[0], [0]], [false, 1]],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("record numeric key selections preserve coercion", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => match({ "1": "value" })
      .with(P.record(P.number.select(), P.string), (keys) => keys)
      .otherwise(() => null);
  `,
    [1],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("record wildcard key selections preserve original and coerced keys", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => match({ "1": "value" })
      .with(P.record(P.select("keys"), P.string.select("values")), (selection) => selection)
      .otherwise(() => null);
  `,
    { keys: [1, "1"], values: ["value"] },
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("prototype selection names preserve the original input callback", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const object = match({ a: 1 })
        .with(P.select("__proto__"), (value) => value)
        .otherwise(() => null);
      const scalar = match(1)
        .with(P.number.select("__proto__"), (value) => value)
        .otherwise(() => null);
      return [object, scalar];
    };
  `,
    [{ a: 1 }, 1],
  )
})

test("record key selections preserve numeric coercion and both key matches", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const numeric = match({ "1": "value" })
        .with(P.record(P.number.select(), P.string), (keys) => keys)
        .otherwise(() => null);
      const wildcard = match({ "1": "value" })
        .with(P.record(P.select("keys"), P.string.select("values")), (selection) => selection)
        .otherwise(() => null);
      return [numeric, wildcard];
    };
  `,
    [[1], { keys: [1, "1"], values: ["value"] }],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("record inherited selection names preserve raw runtime errors", async () => {
  for (const name of [
    "toString",
    "valueOf",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
  ]) {
    const source = `
      import { match, P } from "ts-pattern";
      export const run = () => match({ a: 1 })
        .with(P.record(P.string, P.number.select(${JSON.stringify(name)})), (selection) => selection)
        .otherwise(() => null);
    `
    const raw = await evaluate(source, false)
    const compiled = await evaluate(source, true)
    await assert.rejects(
      async () => raw.run(),
      /concat is not a function/,
      `${name}: raw ts-pattern reference`,
    )
    await assert.rejects(
      async () => compiled.run(),
      /concat is not a function/,
      `${name}: ${compiled.code}`,
    )
  }
})

test("nested record value selections collect selected paths", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const input = {
        a: { name: { first: "John", last: "Doe" } },
        b: { name: { first: "Jane", last: "Doe" } },
      };
      return match(input)
        .with(P.record(P.string, { name: { first: P.string.select() } }), (value) => value)
        .otherwise(() => "no match");
    };
  `,
    ["John", "Jane"],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("map and set matching preserves iterator short-circuiting", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const mapEvents = [];
      class TracedMap extends globalThis.Map {
        entries() {
          const iterator = super.entries();
          return {
            [Symbol.iterator]() { return this; },
            next() { mapEvents.push("next"); return iterator.next(); },
          };
        }
      }
      const map = new TracedMap([["first", 1], ["second", 2]]);
      const mapResult = match(map)
        .with(P.map(
          P.when(() => { mapEvents.push("predicate"); return false; }),
          P.when(() => { mapEvents.push("value"); return true; }),
        ), () => true)
        .otherwise(() => false);

      const setEvents = [];
      class TracedSet extends globalThis.Set {
        [Symbol.iterator]() {
          const iterator = super.values();
          return {
            [Symbol.iterator]() { return this; },
            next() { setEvents.push("next"); return iterator.next(); },
          };
        }
      }
      const set = new TracedSet([1, 2]);
      const setResult = match(set)
        .with(P.set(P.when(() => { setEvents.push("predicate"); return false; })), () => true)
        .otherwise(() => false);
      return [[mapResult, mapEvents], [setResult, setEvents]];
    };
  `,
    [[false, ["next", "predicate", "value"]], [false, ["next", "predicate"]]],
  )
})

test("boolean folding preserves effectful left operands", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let calls = 0;
      const result = match(1)
        .with(P.union(P.when(() => { calls++; return false; }), P._), () => true)
        .otherwise(() => false);
      return [result, calls];
    };
  `,
    [true, 1],
  )
})

test("lambda defaults and destructuring preserve effects", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      let defaults = 0;
      const defaultInput = { value: undefined };
      const defaultResult = match(defaultInput)
        .with({ value: undefined }, ({ value = (() => { defaults++; return 2; })() }) => value)
        .otherwise(() => 0);

      const reads = [];
      const input = {
        get a() { reads.push("a"); return 1; },
        get b() { reads.push("b"); return 2; },
      };
      const destructuredResult = match(input)
        .with({ a: 1, b: 2 }, ({ a, b }) => a + b)
        .otherwise(() => 0);
      return [defaultResult, defaults, destructuredResult, reads];
    };
  `,
    [2, 1, 3, ["a", "b", "a", "b"]],
  )
})

test("regex patterns preserve raw match behavior", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const regex = /a/g;
    export const run = () => {
      const result = match("a")
        .with(P.string.regex(regex), () => true)
        .otherwise(() => false);
      return [result, regex.lastIndex];
    };
  `,
    [true, 0],
  )
})

test("type-only error imports do not become runtime bindings", async () => {
  await assertEquivalent(
    `
    import { match, type NonExhaustiveError } from "ts-pattern";
    export const run = () => {
      try { return match(1).with(2, () => 2).exhaustive(); }
      catch (error) { return error.name; }
    };
  `,
    "Error",
  )
})

test("failed transforms do not leave speculative error imports", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => ((input) =>
      match(input)
        .with({ type: "a" }, () => 1)
        .with({ type: "b", data: P.array(P.select()) }, () => 2)
        .exhaustive())({ type: "a" });
  `,
    1,
  )
  assert.doesNotMatch(code, /NonExhaustiveError/)
})

for (const declaration of ["arrow", "initializer"]) {
  test(`${declaration} sibling cases cache equal nested prefixes in separate bindings`, async () => {
    const match = `match(input)
      .with({ type: "a", data: { k: "1" } }, () => "a1")
      .with({ type: "a", data: { k: "2" } }, () => "a2")
      .with({ type: "b", data: { k: "1" } }, () => "b1")
      .with({ type: "b", data: { k: "2" } }, () => "b2")`
    const code = await assertEquivalent(
      `
      import { match } from "ts-pattern";
      const select = ${
        declaration === "arrow"
          ? `(input) => ${match}.otherwise(() => "none")`
          : `(input) => { try { const result = ${match}.exhaustive(); return result; } catch { return "none"; } }`
      };
      export const run = () => [
        select({ type: "a", data: { k: "2" } }),
        select({ type: "b", data: { k: "1" } }),
        select({ type: "b", data: {} }),
      ];
    `,
      ["a2", "b1", "none"],
    )
    assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
  })
}

for (const declaration of ["arrow", "initializer"]) {
  for (const [selector, handler] of [["P.select()", "(value) => value"], ["P.select('value')", "({ value }) => value"]]) {
    test(`${declaration} ${selector} passes the tested getter value to the handler`, async () => {
      const match = `match(input)
        .with({ kind: "a", value: ${selector} }, ${handler})
        .with({ kind: "b" }, () => 0)`
      const code = await assertEquivalent(
        `
        import { match, P } from "ts-pattern";
        export const run = () => {
          let reads = 0;
          const input = { kind: "a", get value() { return ++reads; } };
          const select = ${
          declaration === "arrow"
            ? `(input) => ${match}.otherwise(() => -1)`
            : `(input) => { const result = ${match}.exhaustive(); return result; }`
        };
          return [select(input), reads];
        };
      `,
        [1, 1],
      )
      assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
    })
  }
}

for (const key of ["P.string", "P.string.select('key')"]) {
  test(`record ${key} keys keep each named value selection separate`, async () => {
    const code = await assertEquivalent(
      `
      import { match, P } from "ts-pattern";
      export const run = () =>
        match({ x: { a: 1, b: 2 }, y: { a: 3, b: 4 } })
          .with(P.record(${key}, { a: P.select("a"), b: P.select("b") }), (selected) => selected)
          .otherwise(() => null);
    `,
      key === "P.string"
        ? { a: [1, 3], b: [2, 4] }
        : { key: ["x", "y"], a: [1, 3], b: [2, 4] },
    )
    assert.doesNotMatch(code, /\.with\(/)
  })
}

test("spread pattern arguments keep their spread meaning", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const options = ["a", "b"];
    export const run = () =>
      ["a", "c"].map((value) => match(value).with(P.union(...options), () => "hit").otherwise(() => "miss"));
  `,
    ["hit", "miss"],
  )
})

test("spread arm arguments keep their spread meaning", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const rest = [() => "number"];
    export const run = () => match(1).with(P.number, ...rest).otherwise(() => "other");
  `,
    "number",
  )
})

test("isMatching compiles direct and curried checks", async () => {
  const code = await assertEquivalent(
    `
    import { isMatching, P } from "ts-pattern";
    const post = { type: "post", title: P.string, stars: P.optional(P.number) };
    const isPost = isMatching(post);
    export const run = () => {
      const inputs = [{ type: "post", title: "a" }, { type: "post", title: 1 }, null, "post"];
      return inputs.map((input) => [isMatching(post, input), isPost(input), isMatching(P.string.startsWith("p"), input)]);
    };
  `,
    [[true, true, false], [false, false, false], [false, false, false], [false, false, true]],
  )
  assert.doesNotMatch(code, /isMatching\)?\(/, "isMatching calls must be compiled")
})

test("chained optional, and, or modifiers compile", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const classify = (input) =>
      match(input)
        .with({ stars: P.number.between(1, 5).optional(), tag: P.string.or(P.number) }, () => "rated")
        .with({ name: P.string.startsWith("a").and(P.string.endsWith("z")) }, () => "az")
        .otherwise(() => "other");
    export const run = () => [
      classify({ tag: "t" }),
      classify({ stars: 3, tag: 1 }),
      classify({ stars: 9, tag: 1 }),
      classify({ stars: undefined, tag: true }),
      classify({ name: "abcz" }),
      classify({ name: "abc" }),
    ];
  `,
    ["rated", "rated", "other", "other", "az", "other"],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("chained modifiers evaluate their base pattern values once", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let reads = 0;
      const ctor = () => { reads++; return Date; };
      const results = [new Date(0), 1].map((input) =>
        match(input).with(P.instanceOf(ctor()).optional(), () => "date").otherwise(() => "other"));
      return [results, reads];
    };
  `,
    [["date", "other"], 2],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("modifiers missing from a pattern keep the runtime error", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      try {
        const input = [1];
        const isOne = (value) => value === 1;
        return [
          match(input).with(P.array(P.number).and(P.array()), () => "both").otherwise(() => "other"),
          match(1).with(P.when(isOne).or(2), () => "one").otherwise(() => "other"),
        ];
      } catch (error) {
        return error.constructor.name;
      }
    };
  `,
    "TypeError",
  )
})

test("const pattern aliases resolve through chained modifiers", async () => {
  const code = await assertEquivalent(
    `
    import { isMatching, P } from "ts-pattern";
    const post = { stars: P.number.between(1, 5).optional(), title: P.string };
    export const run = () =>
      [{ title: "a" }, { title: "a", stars: 3 }, { title: "a", stars: 7 }].map((input) => {
        const response = { data: [input] };
        return isMatching({ data: P.array(post) }, response) && isMatching(post, input);
      });
  `,
    [true, true, false],
  )
  assert.doesNotMatch(code, /isMatching\)?\(\{/, "aliased chained patterns must compile")
})
