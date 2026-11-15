import assert from "node:assert/strict"
import { test } from "node:test"
import { compile, load, require, versions } from "./harness.mjs"

const evaluate = async (source, plugin, version = versions.at(-1)) => {
  const code = await compile(source, plugin)
  return { code, run: load(code, version) }
}

// Runs on every version that exports each `P.<name>` the source uses.
const versionsFor = (source) => {
  const names = [...source.matchAll(/\bP\.(\w+)/g)].map(([, name]) => name)
  return versions.filter((version) => names.every((name) => name in require(version).P))
}

const assertEquivalent = async (source, expected) => {
  const baseline = await compile(source, false)
  const compiled = await compile(source, true)
  for (const version of versionsFor(source)) {
    assert.deepEqual(load(baseline, version)(), expected, `${version}: raw ts-pattern reference`)
    assert.deepEqual(load(compiled, version)(), expected, `${version}: ${compiled}`)
  }
  return compiled
}

// Ordinary functions are intentional here: callback arguments and dynamic this
// are observable parts of the ts-pattern protocol, unlike arrow functions.
for (const { name, input, chain, declaration, expected } of [
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
    chain: ".with({ kind: 'a' }, handler).with({ kind: 'b' }, () => 0).otherwise(() => 0)",
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
    chain: ".with({ kind: 'a' }, handler).with({ kind: 'b' }, () => 0).exhaustive()",
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
]) {
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
    assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
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
          .otherwise(${terminal === "otherwise" ? "callbacks.handler" : "() => 0"});
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
    const terminal = declaration === "assignment" ? "exhaustive()" : "otherwise(() => 'other')"
    const scalar = `match(input).with(0, () => 'positive').with(-0, () => 'negative').${terminal}`
    const object = `match(input).with({ value: 0 }, () => 'positive').with({ value: -0 }, () => 'negative').${terminal}`
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

// P.when(f) has no chain methods and P.array(x) has no and/or, so ts-pattern
// throws a TypeError while it builds the pattern, e.g. on P.when(f).select().
for (const pattern of [
  "{ a: P.when(() => true).select() }",
  "P.when(() => true).optional()",
  "P.array(P.number).and(P.any)",
  "P.array(P.number).or(P.any)",
]) {
  test(`${pattern} throws like ts-pattern`, async () => {
    const source = `
    import { match, P } from "ts-pattern";
    export const run = () => {
      try {
        return match({ a: 1 }).with(${pattern}, () => "matched").otherwise(() => "other");
      } catch (error) {
        return error.constructor.name;
      }
    };
  `
    await assertEquivalent(source, "TypeError")
  })
}

test("evaluates later arm arguments after getters of earlier arms", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const log = [];
    const handler = (value) => (log.push("argument"), value);
    export const run = () => {
      log.length = 0;
      const input = { get a() { log.push("get a"); return 0; }, b: [] };
      const result = match(input)
        .with({ a: 1 }, () => "a")
        .with({ b: [] }, handler(() => "b"))
        .otherwise(() => 0);
      return [result, log];
    };
  `,
    ["b", ["get a", "argument"]],
  )
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
    [
      [false, ["first"]],
      [false, ["first"]],
      ["1", 1],
      ["1", 1],
      ["1", 1],
    ],
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
    [
      [true, [0]],
      [[0], [0]],
      [false, 1],
    ],
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
    [
      [false, ["next", "predicate", "value"]],
      [false, ["next", "predicate"]],
    ],
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

// 5.0-5.2 throw a plain Error and do not export NonExhaustiveError.
for (const [binding, match] of [
  ["{ match }", "match"],
  ["* as TP", "TP.match"],
]) {
  test(`unmatched exhaustive through ${match} throws each version's error`, async () => {
    const source = `
      import ${binding} from "ts-pattern";
      export const run = () => {
        try { return ${match}({ a: 1 }).with({ a: 2 }, () => 2).exhaustive(); }
        catch (error) { return [error.constructor.name, error.message, error.input]; }
      };
    `
    const baseline = await compile(source, false)
    const compiled = await compile(source, true)
    assert.doesNotMatch(compiled, /\.with\(/)
    for (const version of versions) {
      assert.deepEqual(
        load(compiled, version)(),
        load(baseline, version)(),
        `${version}: ${compiled}`,
      )
    }
  })
}

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
  for (const [selector, handler] of [
    ["P.select()", "(value) => value"],
    ["P.select('value')", "({ value }) => value"],
  ]) {
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
      key === "P.string" ? { a: [1, 3], b: [2, 4] } : { key: ["x", "y"], a: [1, 3], b: [2, 4] },
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
    [
      [true, true, false],
      [false, false, false],
      [false, false, false],
      [false, false, true],
    ],
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

test("records with number-rejecting key patterns skip key coercion", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const classify = (input) =>
      match(input)
        .with(P.record(P.string, P.number), () => "scores")
        .with(P.record(P.union("a", "b"), P.string), () => "ab")
        .with(P.record(P.string.select(), P.boolean), (keys) => keys)
        .otherwise(() => "other");
    export const run = () => [
      classify({ 1: 2, a: 3 }),
      classify({ a: "x", 2: "y" }),
      classify({ a: "x", b: "y" }),
      classify({ 7: true, k: false }),
    ];
  `,
    ["scores", "other", "ab", ["7", "k"]],
  )
  assert.doesNotMatch(code, /\.with\(/)
  assert.doesNotMatch(code, /Number\(/, "number-rejecting key patterns need no coercion")
})

test("inlined callbacks bind their arguments before the body runs", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let input = 1;
      let object = { n: 1 };
      const bumpInput = () => { input = 2; return 0; };
      const bumpObject = () => { object = { n: 2 }; return 0; };
      const direct = match(input).with(P.number, (value) => bumpInput() + value).otherwise(() => -1);
      const destructured = match(object).with({ n: P.number }, ({ n }) => bumpObject() + n).otherwise(() => -1);
      return [direct, destructured];
    };
  `,
    [1, 1],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("inlined callbacks substitute shorthand properties", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const pick = (input) => match(input).with({ error: P.select() }, (error) => ({ error })).otherwise(() => null);
      return [pick({ error: 1 }), match(2).with(P.number, (value) => ({ value, twice: value * 2 })).otherwise(() => null)];
    };
  `,
    [{ error: 1 }, { value: 2, twice: 4 }],
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("only a three-argument with treats its middle function as a guard", async () => {
  const code = await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      const input = 1;
      return [
        match(input).with(1, 2, () => false, () => "pattern").otherwise(() => "other"),
        match(input).with(1, () => false, () => "guard").otherwise(() => "other"),
      ];
    };
  `,
    ["pattern", "other"],
  )
  return code
})

for (const declaration of ["arrow", "ternary", "initializer"]) {
  test(`${declaration} nested patterns read each property once`, async () => {
    const match = `match(input)
      .with({ a: { b: 1, c: P.union(2, 3) }, d: P.nullish }, () => "nested")
      .with({ a: { b: P.select() } }, (b) => b)`
    const code = await assertEquivalent(
      `
      import { match, P } from "ts-pattern";
      export const run = () => {
        const reads = [];
        const input = {
          get a() { reads.push("a"); return { get b() { reads.push("b"); return 1; }, c: 4 }; },
          get d() { reads.push("d"); return null; },
        };
        const select = ${
          declaration === "arrow"
            ? `(input) => ${match}.otherwise(() => "none")`
            : declaration === "ternary"
              ? `(input) => [${match}.otherwise(() => "none")][0]`
              : `(input) => { const result = ${match}.exhaustive(); return result; }`
        };
        return [select(input), reads];
      };
    `,
      [1, ["a", "b", "a", "b"]],
    )
    assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
  })
}

test("hoisted expression temporaries survive re-entrant matches", async () => {
  const code = await assertEquivalent(
    `
    import { isMatching, match, P } from "ts-pattern";
    // The predicate re-enters the match before the outer test reads its
    // temporary for \`child\` again.
    const check = (node) => [
      match(node)
        .with({ child: { deeper: P.when((deeper) => { if (deeper) check(deeper); return true; }), tag: "x" } }, () => "ok")
        .otherwise(() => "bad"),
    ][0];
    const holds = (node) =>
      isMatching({ child: { deeper: P.when((deeper) => { if (deeper) holds(deeper); return true; }), tag: "x" } }, node);
    export const run = () => {
      const tree = { child: { deeper: { child: { deeper: null, tag: "y" } }, tag: "x" } };
      return [check(tree), holds(tree)];
    };
  `,
    ["ok", true],
  )
  assert.doesNotMatch(code, /\.with\(/)
  assert.doesNotMatch(code, /\)\(undefined/, "function bodies declare the temporaries")
})

test("const object patterns that escape are not inlined", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const escaped = { type: "a" };
    const inner = { type: "a" };
    const outer = { inner: inner };
    const holder = { value: outer };
    const mutate = (value) => { value.type = "b"; };
    mutate(escaped);
    mutate(holder.value.inner);
    const kept = { type: "a" };
    export const run = () => {
      const direct = (input) => match(input).with(escaped, () => "escaped").otherwise(() => "other");
      const transitive = (input) => match(input).with(outer, () => "transitive").otherwise(() => "other");
      const pattern = (input) => match(input).with(P.union(kept), () => "kept").otherwise(() => "other");
      return [direct({ type: "b" }), transitive({ inner: { type: "b" } }), pattern({ type: "a" })];
    };
  `,
    ["escaped", "transitive", "kept"],
  )
  assert.match(code, /=== "a"/, "patterns used only as patterns still compile")
})

test("local undefined bindings match their own value", async () => {
  await assertEquivalent(
    `
    import { match } from "ts-pattern";
    export const run = () => {
      const undefined = 1;
      return [
        match(1).with(undefined, () => "local").otherwise(() => "other"),
        match({ a: 1 }).with({ a: undefined }, () => "local").otherwise(() => "other"),
      ];
    };
  `,
    ["local", "local"],
  )
})

test("object patterns keep the keys their literal creates", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const key = "a";
    export const run = () => [
      match({ a: 1 }).with({ __proto__: P.string }, () => "string").otherwise(() => "other"),
      match({ a: 1 }).with({ "__proto__": 1 }, () => "no key").otherwise(() => "other"),
      match({ a: 2 }).with({ a: 1, a: 2 }, () => "last").otherwise(() => "other"),
      match({ 1: "y" }).with({ 1: "x", "1": "y" }, () => "last").otherwise(() => "other"),
      match({ a: 2 }).with({ [key]: 1, a: 2 }, () => "last").otherwise(() => "other"),
      match({ a: { b: 2 } }).with({ a: { b: 1, b: 2 } }, () => "last").otherwise(() => "other"),
    ];
  `,
    // The first pattern inherits P.string matching through its prototype.
    ["other", "no key", "last", "last", "last", "last"],
  )
})

test("selections read their value only after the pattern matches its type", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const pick = (input) =>
      match(input)
        .with([P.select()], (s) => ["one", s])
        .with([P.select("h"), P.select("t")], (s) => s)
        .otherwise(() => "other");
    export const run = () => [pick(null), pick(undefined), pick(["a"]), pick(["a", "b"])];
  `,
    ["other", "other", ["one", "a"], { h: "a", t: "b" }],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("selection names that are not identifiers become quoted keys", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const quoted = (input) => match(input).with({ a: P.select("my-name") }, (s) => s).otherwise(() => "other");
    const spaced = (input) => match(input).with({ a: P.select("a b"), b: P.select("0") }, (s) => s).otherwise(() => "other");
    const escaped = (input) => match(input).with({ a: P.select(\`x\\ny\`) }, (s) => s).otherwise(() => "other");
    const record = (input) => match(input).with(P.record(P.select("k-1"), P.number.select("v 1")), (s) => s).otherwise(() => "other");
    export const run = () => [quoted({ a: 1 }), spaced({ a: 1, b: 2 }), escaped({ a: 1 }), record({ x: 1 })];
  `,
    [{ "my-name": 1 }, { "a b": 1, 0: 2 }, { "x\ny": 1 }, { "k-1": ["x"], "v 1": [1] }],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("array destructuring handlers iterate their input", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const first = (input) => match(input).with(P.string, () => "string").otherwise(([item]) => item);
    export const run = () => {
      let error;
      try { first({ 0: "object" }); } catch (caught) { error = caught.constructor.name; }
      return [first(new Set(["set"])), first(new Map([[1, 2]])), first(["array"]), error];
    };
  `,
    ["set", [1, 2], "array", "TypeError"],
  )
})

test("records that select nothing pass the input to the handler", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const values = (input) => match(input).with(P.record(P.string, P.select()), (s) => s).otherwise(() => "other");
    const entries = (input) =>
      match(input).with(P.record(P.select("k"), P.number.select("v")), (s) => s).otherwise(() => "other");
    const nested = (input) => match(input).with({ r: P.record(P.string, P.select()) }, (s) => s).otherwise(() => "other");
    export const run = () => [values({}), values({ a: 1 }), entries({}), entries({ a: 1 }), nested({ r: {} })];
  `,
    [{}, [1], {}, { k: ["a"], v: [1] }, { r: {} }],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const mixed = (input) =>
      match(input).with({ r: P.record(P.string, P.select("v")), s: P.select("s") }, (s) => s).otherwise(() => "other");
    export const run = () => [mixed({ r: {}, s: 1 }), mixed({ r: { a: 2 }, s: 1 })];
  `,
    [{ s: 1 }, { v: [2], s: 1 }],
  )
})

test("arms with several patterns pass only the selections of the pattern that matched", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const named = (input) =>
      match(input).with({ a: P.select("x"), b: 1 }, { c: P.select("y") }, (s) => s).otherwise(() => "other");
    const anonymous = (input) =>
      match(input).with({ a: P.select() }, { b: P.select() }, (s) => s).otherwise(() => "other");
    const partly = (input) =>
      match(input).with({ a: P.select() }, { b: 1 }, (s) => s).otherwise(() => "other");
    export const run = () => [
      named({ a: 1, b: 1 }), named({ c: 3 }), named({ a: 1, b: 2, c: 3 }),
      anonymous({ a: 1 }), anonymous({ b: 2 }),
      partly({ a: 1 }), partly({ b: 1 }),
    ];
  `,
    [{ x: 1 }, { y: 3 }, { x: 1, y: 3 }, 1, 2, 1, { b: 1 }],
  )
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const anonymous = (input) =>
      match(input).with({ a: P.select(), b: 1 }, { c: P.select() }, (s) => s).otherwise(() => "other");
    const named = (input) =>
      match(input).with({ a: P.select("x") }, [P.select("x"), 2], (s) => s).otherwise(() => "other");
    export const run = () => [anonymous({ a: 1, b: 1 }), anonymous({ a: 1, b: 2, c: 3 }), named({ a: 1 }), named([4, 2])];
  `,
    [1, 3, { x: 1 }, { x: 4 }],
  )
  assert.doesNotMatch(
    code,
    /\.with\(/,
    "arms whose patterns select the same names must be compiled",
  )
})

test("optional selections read their property once", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const reads = [];
      const input = { get a() { reads.push("a"); return { get b() { reads.push("b"); return 3; } }; } };
      const pick = (input) => match(input).with({ a: P.optional(P.select()) }, (s) => s).otherwise(() => "other");
      const nested = (input) =>
        match(input).with({ a: P.optional({ b: P.select() }) }, (s) => [s, s]).otherwise(() => "other");
      return [typeof pick(input), nested(input), nested({}), reads];
    };
  `,
    ["object", [3, 3], [undefined, undefined], ["a", "a", "b"]],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("object patterns test integer keys before string keys", async () => {
  const code = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const log = [];
      const f = (input) =>
        match(input)
          .with({ b: P.when(() => log.push("b")), 1: P.when(() => log.push("1")), a: P.when(() => log.push("a")), 0: P.when(() => log.push("0")) }, () => "hit")
          .otherwise(() => "miss");
      return [f({ a: 1, b: 1, 0: 1, 1: 1 }), log];
    };
  `,
    ["hit", ["0", "1", "b", "a"]],
  )
  assert.doesNotMatch(code, /\.with\(/, "the supported chain must be compiled")
})

test("arms keep predicates on keys before the discriminant", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const log = [];
      const f = (input) =>
        match(input)
          .with({ x: P.when(() => (log.push("x"), true)), type: "a" }, () => "a")
          .with({ type: "b" }, () => "b")
          .otherwise(() => "miss");
      return [f({ type: "b", x: 1 }), f({ type: "a", x: 1 }), log];
    };
  `,
    ["b", "a", ["x", "x"]],
  )
})

test("later patterns are built after earlier handlers run", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const log = [];
      const make = (n) => (log.push("make " + n), (v) => v === n);
      const f = (input) =>
        match(input).with(P.when(make(1)), () => (log.push("h1"), 1)).with(P.when(make(2)), () => (log.push("h2"), 2)).otherwise(() => 0);
      const g = (input) =>
        match(input).with(1, () => { throw new Error("boom") }).with(P.instanceOf((log.push("ctor"), Error)), () => 2).otherwise(() => 0);
      let error;
      try { g(1) } catch (e) { error = e.message }
      return [f(1), f(2), error, log];
    };
  `,
    [1, 2, "boom", ["make 1", "h1", "make 2", "make 1", "make 2", "h2"]],
  )
})

test("records under other patterns give their selections, not their result", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const optional = (input) =>
      match(input).with({ r: P.optional(P.record(P.string, P.select())) }, (s) => s).otherwise(() => "other");
    const nested = (input) =>
      match(input).with(P.record(P.string, P.record(P.string, P.select())), (s) => s).otherwise(() => "other");
    export const run = () => [optional({}), optional({ r: {} }), optional({ r: { a: 1 } }), nested({ a: {} }), nested({ a: { b: 1 } })];
  `,
    [undefined, { r: {} }, [1], { a: {} }, [[1]]],
  )
})

test("later arm arguments read bindings after earlier arms change them", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      let handler = () => "old";
      let prefix = "a";
      const log = [];
      const changed = match("bc")
        .with(P.when(() => { handler = () => "new"; prefix = "b"; return false; }), () => "x")
        .with(P.string.startsWith(prefix), handler)
        .otherwise(() => "other");
      const defaults = match({})
        .with({ a: P.optional(P.select()) }, (s = log.push("default")) => "x")
        .with(P.when((log.push("build"), () => true)), () => "y")
        .otherwise(() => "other");
      return [changed, defaults, log];
    };
  `,
    ["new", "x", ["default", "build"]],
  )
})

test("later instanceOf arms read classes after earlier arms reassign them", async () => {
  const fixed = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      class A {}
      class B {}
      return match(new B())
        .with(P.instanceOf(A), () => { return "a"; })
        .with(P.instanceOf(B), () => { return "b"; })
        .otherwise(() => "other");
    };
  `,
    "b",
  )
  assert.doesNotMatch(fixed, /\.with\(/)
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => {
      const log = [];
      const get = () =>
        match(1)
          .with(P.when(() => { log.push("guard"); return false; }), () => "x")
          .with(P.instanceOf(Later), () => "later")
          .otherwise(() => "other");
      let result;
      try { result = get(); } catch (error) { result = error.constructor.name; }
      class Later {}
      return [result, log];
    };
  `,
    ["ReferenceError", ["guard"]],
  )
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    class A {}
    const log = [];
    const early = (() => { try { return get(); } catch (error) { return error.constructor.name; } })();
    class Later {}
    function get() {
      return match(1)
        .with(P.when(() => { log.push("guard"); return false; }), () => "x")
        .with(P.instanceOf(Later), () => "later")
        .otherwise(() => "other");
    }
    const skipped = (() => {
      let result;
      try {
        switch (1) {
          case 0: const Skipped = A; break;
          case 1: result = match(1)
            .with(P.when(() => { log.push("case"); return false; }), () => "x")
            .with(P.instanceOf(Skipped), () => "skipped")
            .otherwise(() => "other");
        }
      } catch (error) { result = error.constructor.name; }
      return result;
    })();
    export const run = () => [early, skipped, log];
  `,
    ["ReferenceError", "ReferenceError", ["guard", "case"]],
  )
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    class A {}
    class B {}
    function C() {}
    export const run = () => {
      let it;
      function* gen() {
        function D() {}
        yield () => match(new A())
          .with(P.when(() => { it.next(); return false; }), () => "x")
          .with(P.instanceOf(D), () => "d")
          .otherwise(() => "other");
        var D = A;
        yield;
      }
      it = gen();
      const redeclared = it.next().value();
      const loops = match(new A())
        .with(P.when(() => { for (B of [A]); for ([C] of [[A]]); return false; }), () => "x")
        .with(P.instanceOf(B), P.instanceOf(C), () => "loop")
        .otherwise(() => "other");
      return [redeclared, loops];
    };
  `,
    ["d", "loop"],
  )
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    class A {}
    class B {}
    export const run = () => {
      const log = [];
      globalThis.TsPatternGlobal = B;
      const changed = match(new A())
        .with(P.when(() => { globalThis.TsPatternGlobal = A; return false; }), () => "x")
        .with(P.instanceOf(TsPatternGlobal), () => "global")
        .otherwise(() => "other");
      let missing;
      try {
        missing = match(1)
          .with(P.when(() => { log.push("guard"); return false; }), () => "x")
          .with(P.instanceOf(TsPatternMissing), () => "missing")
          .otherwise(() => "other");
      } catch (error) { missing = error.constructor.name; }
      delete globalThis.TsPatternGlobal;
      return [changed, missing, log];
    };
  `,
    ["global", "ReferenceError", ["guard"]],
  )
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    class A {}
    class B {}
    export const run = () =>
      match(new A())
        .with(P.when(() => { B = A; return false; }), () => { return "x"; })
        .with(P.instanceOf(B), () => { return "b"; })
        .otherwise(() => "other");
  `,
    "b",
  )
})

// `.exhaustive(handler)` calls `handler` only on 5.7 and later, so each
// version compares with its own raw run.
test("terminal methods compile and match the runtime on every version", async () => {
  const source = `
    import { match, P } from "ts-pattern";
    const attempt = (run) => { try { return run(); } catch (error) { return error.constructor.name; } };
    export const run = () => [1, "a", null].map((input) => {
      const log = [];
      const show = (value) => value;
      return [
        attempt(() => {
          match(input).with(1, () => log.push("statement")).exhaustive();
          return log;
        }),
        attempt(() => match(input).with(1, () => "run").run()),
        attempt(() => match(input).returnType().with(1, () => "typed").exhaustive()),
        attempt(() => match(input).with(1, () => "handled").exhaustive((value) => ["unmatched", value])),
        attempt(() => show(match(input).with(1, () => "argument").with(P.string, () => "string").exhaustive())),
      ];
    });
  `
  const [raw, compiled] = await Promise.all([compile(source, false), compile(source, true)])
  assert.doesNotMatch(compiled, /\.with\(/, compiled)
  for (const version of versions) {
    assert.deepEqual(load(compiled, version)(), load(raw, version)(), `${version}: ${compiled}`)
  }
})

// ts-pattern reads `length`, then calls `slice` for the head, the tail and the
// rest, in that order, on a variadic tuple.
test("variadic tuples compile and slice their input like ts-pattern", async () => {
  const compiled = await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    export const run = () => [[], [1], [1, "a"], [1, 2, "a"], [1, "x", 2, true], [2, 3]].map((items) => {
      const log = [];
      const input = new Proxy(items, {
        get: (target, key, receiver) => {
          if (typeof key === "string") log.push(key);
          return Reflect.get(target, key, receiver);
        },
      });
      const result = match(input)
        .with([1, ...P.array(P.number), "a"], () => "both")
        .with([...P.array(), true], () => "tail")
        .with([P.select(), ...P.array(P.number)], (head) => ["head", head])
        .with([...P.array(P.string)], () => "strings")
        .otherwise(() => "other");
      return [result, log];
    });
  `,
    [
      [
        "strings",
        [
          "length",
          "length",
          "length",
          "length",
          "slice",
          "length",
          "constructor",
          "slice",
          "length",
          "constructor",
        ],
      ],
      [
        ["head", 1],
        [
          "length",
          "length",
          "slice",
          "length",
          "constructor",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
        ],
      ],
      [
        "both",
        [
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "1",
          "slice",
          "length",
          "constructor",
        ],
      ],
      [
        "both",
        [
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "2",
          "slice",
          "length",
          "constructor",
          "1",
        ],
      ],
      [
        "tail",
        [
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "3",
          "slice",
          "length",
          "constructor",
          "1",
          "2",
          "length",
          "slice",
          "length",
          "constructor",
          "slice",
          "length",
          "constructor",
          "3",
          "slice",
          "length",
          "constructor",
          "0",
          "1",
          "2",
        ],
      ],
      [
        ["head", 2],
        [
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "1",
          "slice",
          "length",
          "constructor",
          "length",
          "slice",
          "length",
          "constructor",
          "slice",
          "length",
          "constructor",
          "1",
          "slice",
          "length",
          "constructor",
          "0",
          "length",
          "slice",
          "length",
          "constructor",
          "0",
          "slice",
          "length",
          "constructor",
          "1",
        ],
      ],
    ],
  )
  assert.doesNotMatch(compiled, /\.with\(/, compiled)
})

test("P.array patterns spread elsewhere stay variadic", async () => {
  await assertEquivalent(
    `
    import { match, P } from "ts-pattern";
    const rest = P.array(P.number);
    export const run = () => [
      match([1, 2]).with([1, ...rest], () => "spread").otherwise(() => "other"),
      match([1, 2]).with([rest], () => "variadic").otherwise(() => "other"),
    ];
  `,
    ["spread", "variadic"],
  )
})
