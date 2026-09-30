# @scarf005/ts-pattern-swc-plugin

SWC plugin that compiles supported [`ts-pattern`](https://github.com/gvergnaud/ts-pattern) `match` expressions into plain JavaScript ternaries, `switch` statements, or `if` chains.

```ts
import { match, P } from "ts-pattern";

const label = match(input)
  .with({ type: "ok", value: P.number }, (value) => value.value)
  .with({ type: "error" }, () => 0)
  .otherwise(() => -1);
```

The plugin emits ternaries, `switch` statements, or ordered `if` blocks, preserving input evaluation and callback arguments. Unsupported chains retain the original ts-pattern runtime.

## Install

From npm:

```sh
npm install @scarf005/ts-pattern-swc-plugin
```

Or from JSR:

```sh
npx jsr add @scarf005/ts-pattern-swc-plugin
```

## Configure SWC

Use `@swc/core@1.15.33`, the runtime matched to this plugin's WASM ABI. Resolve the WASM explicitly rather than using the package root: the JSR/npm bridge root exports the transform API, while native npm archives use a WASM root.

```ts
import { transform } from "@swc/core";
import { defaultPluginPath } from "@scarf005/ts-pattern-swc-plugin/transform";

const result = await transform(source, {
  filename: "input.ts",
  jsc: {
    parser: { syntax: "typescript" },
    experimental: { plugins: [[await defaultPluginPath(), {}]] },
  },
});
```

## Run TypeScript directly

The loader compiles local `.ts`, `.tsx`, `.mts`, and `.cts` files before evaluation. It uses synchronous `node:module.registerHooks()` on Node 24+ and Deno 2.9.7+. Remote application modules are not transformed.

### Node.js

Install the plugin and ts-pattern:

```sh
npm install ts-pattern @scarf005/ts-pattern-swc-plugin
```

Preload the loader:

```sh
node --import @scarf005/ts-pattern-swc-plugin/register ./foo.ts
```

### Deno

Use `import { match } from "ts-pattern"` with a `deno.json` import map, or `import { match } from "npm:ts-pattern@5.9.0"`. Run through the CLI:

```sh
deno run -A jsr:@scarf005/ts-pattern-swc-plugin/run ./foo.ts
```

The CLI imports the entry as a module. Use preload when your entry relies on native main-module identity (`import.meta.main` or `require.main`).

```sh
deno run -A --preload jsr:@scarf005/ts-pattern-swc-plugin/register ./foo.ts
```

Native JSR imports download the package's WASM into a content-addressed local cache before registering the loader; downloads are limited to 64 MiB, damaged cache files are replaced atomically, and download or transform failures abort execution. This requires network, filesystem and native SWC permissions.

Deno requires explicit extensions for relative imports. Import generated `.cts` modules through their default export, then destructure the exported members; Deno cannot discover the SWC-generated named CommonJS exports during static analysis.

For repository-local development, build with `cd plugin && npm ci && npm run build`, then run from the repository root using the examples import map:

```sh
deno run -A --config examples/deno.jsonc --preload ./plugin/register.mjs ./examples/literals.ts
```

### Programmatic transform

```ts
import { transformTsPattern } from "@scarf005/ts-pattern-swc-plugin/transform";

const result = await transformTsPattern(source, { filename: "foo.ts" });
console.log(result.code);
```

`defaultPluginPath()` is asynchronous so it can materialize a remote JSR WASM. `transformTsPattern()` prepares it automatically. `transformTsPatternSync()` accepts the same options; for remote imports, first await `defaultPluginPath()` or pass an explicit `pluginPath`.

Set `TS_PATTERN_SWC_PLUGIN_PATH` to use a custom-built WASM with the loader. For programmatic transforms, pass `pluginPath`. Restart the process after rebuilding a custom WASM: SWC caches loaded plugins for the process lifetime.

## Benchmark

The `bench` task compares [`bdbaraban/ts-pattern-benchmark`](https://github.com/bdbaraban/ts-pattern-benchmark) cases across raw `ts-pattern`, native code, and plugin output, then runs the local examples.

<!-- automd:file src="../docs/just-bench.txt" code lang="text" -->

```text [just-bench.txt]
CPU | AMD Ryzen 5 5600G with Radeon Graphics
Runtime | Deno 2.9.7 (x86_64-unknown-linux-gnu)

file:///home/scarf/repo/ts-pattern-swc-plugin/web/ts-pattern_bench.ts

| benchmark               | time/iter (avg) |        iter/s |      (min … max)      |      p75 |      p99 |     p995 |
| ----------------------- | --------------- | ------------- | --------------------- | -------- | -------- | -------- |

group ts-pattern-benchmark/always-last-digit
| ts-pattern raw          |          1.2 µs |       819,400 | (818.3 ns …   1.7 µs) |   1.4 µs |   1.7 µs |   1.7 µs |
| native code             |         16.9 ns |    59,140,000 | ( 12.2 ns … 135.4 ns) |  21.3 ns |  26.6 ns |  28.5 ns |
| ts-pattern swc plugin   |         18.4 ns |    54,270,000 | ( 13.0 ns … 298.2 ns) |  22.9 ns |  31.6 ns |  39.8 ns |

summary
  native code
     1.09x faster than ts-pattern swc plugin
    72.17x faster than ts-pattern raw

group ts-pattern-benchmark/random-digit
| ts-pattern raw          |        792.1 ns |     1,262,000 | (516.8 ns …   1.1 µs) | 889.4 ns |   1.1 µs |   1.1 µs |
| native code             |         20.9 ns |    47,890,000 | ( 12.4 ns … 223.0 ns) |  23.3 ns |  63.2 ns |  96.1 ns |
| ts-pattern swc plugin   |         17.8 ns |    56,120,000 | ( 12.9 ns … 233.0 ns) |  21.4 ns |  33.1 ns |  42.9 ns |

summary
  native code
     1.17x slower than ts-pattern swc plugin
    37.93x faster than ts-pattern raw

group ts-pattern-benchmark/nested-objects
| ts-pattern raw          |          2.4 µs |       416,100 | (  2.0 µs …   2.9 µs) |   2.6 µs |   2.9 µs |   2.9 µs |
| native code             |         22.1 ns |    45,190,000 | ( 15.6 ns … 251.1 ns) |  27.4 ns |  38.2 ns |  43.8 ns |
| ts-pattern swc plugin   |         20.7 ns |    48,400,000 | ( 13.5 ns … 328.6 ns) |  25.5 ns |  48.2 ns |  76.7 ns |

summary
  native code
     1.07x slower than ts-pattern swc plugin
   108.60x faster than ts-pattern raw

group intro-html
| ts-pattern raw          |          2.1 µs |       468,000 | (  1.7 µs …   2.7 µs) |   2.3 µs |   2.7 µs |   2.7 µs |
| ts-pattern swc plugin   |         67.4 ns |    14,830,000 | ( 51.7 ns … 143.3 ns) |  73.8 ns | 115.9 ns | 121.1 ns |

summary
  ts-pattern swc plugin
    31.69x faster than ts-pattern raw

group is-matching-curried
| ts-pattern raw          |        504.5 ns |     1,982,000 | (378.4 ns … 771.1 ns) | 565.9 ns | 736.0 ns | 771.1 ns |
| ts-pattern swc plugin   |        470.2 ns |     2,127,000 | (372.6 ns … 793.5 ns) | 501.3 ns | 726.8 ns | 793.5 ns |

summary
  ts-pattern swc plugin
     1.07x faster than ts-pattern raw

group is-matching-direct
| ts-pattern raw          |        546.9 ns |     1,829,000 | (366.4 ns … 860.9 ns) | 626.2 ns | 822.6 ns | 860.9 ns |
| ts-pattern swc plugin   |        571.9 ns |     1,749,000 | (383.6 ns … 999.5 ns) | 634.1 ns | 999.5 ns | 999.5 ns |

summary
  ts-pattern swc plugin
     1.05x slower than ts-pattern raw

group literals
| ts-pattern raw          |        617.5 ns |     1,619,000 | (406.3 ns …   1.1 µs) | 706.5 ns |   1.1 µs |   1.1 µs |
| ts-pattern swc plugin   |         23.5 ns |    42,640,000 | ( 16.8 ns …  68.4 ns) |  29.6 ns |  43.4 ns |  46.1 ns |

summary
  ts-pattern swc plugin
    26.33x faster than ts-pattern raw

group matching-several-patterns
| ts-pattern raw          |        670.9 ns |     1,490,000 | (392.3 ns …   1.6 µs) | 740.5 ns |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         33.3 ns |    30,040,000 | ( 21.0 ns … 158.1 ns) |  40.1 ns |  60.1 ns |  69.2 ns |

summary
  ts-pattern swc plugin
    20.16x faster than ts-pattern raw

group objects
| ts-pattern raw          |          1.0 µs |       967,400 | (741.3 ns …   1.5 µs) |   1.1 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         93.2 ns |    10,730,000 | ( 37.6 ns … 674.0 ns) |  87.2 ns | 372.8 ns | 461.7 ns |

summary
  ts-pattern swc plugin
    11.09x faster than ts-pattern raw

group p-array-patterns
| ts-pattern raw          |          2.2 µs |       451,300 | (  1.4 µs …   3.6 µs) |   2.4 µs |   3.6 µs |   3.6 µs |
| ts-pattern swc plugin   |         44.0 ns |    22,720,000 | ( 26.7 ns … 217.0 ns) |  52.0 ns |  89.3 ns | 103.5 ns |

summary
  ts-pattern swc plugin
    50.34x faster than ts-pattern raw

group p-bigint-wildcard
| ts-pattern raw          |        162.6 ns |     6,150,000 | ( 83.9 ns … 559.7 ns) | 181.9 ns | 439.1 ns | 498.0 ns |
| ts-pattern swc plugin   |         18.2 ns |    54,880,000 | ( 12.1 ns … 127.7 ns) |  22.2 ns |  32.7 ns |  35.8 ns |

summary
  ts-pattern swc plugin
     8.92x faster than ts-pattern raw

group p-boolean-wildcard
| ts-pattern raw          |        369.2 ns |     2,708,000 | (201.3 ns … 629.2 ns) | 421.4 ns | 590.2 ns | 629.2 ns |
| ts-pattern swc plugin   |         22.1 ns |    45,310,000 | ( 11.7 ns … 403.4 ns) |  23.8 ns |  90.3 ns | 155.1 ns |

summary
  ts-pattern swc plugin
    16.73x faster than ts-pattern raw

group p-infer
| ts-pattern raw          |          3.4 µs |       294,900 | (  2.4 µs …   5.7 µs) |   3.7 µs |   5.7 µs |   5.7 µs |
| ts-pattern swc plugin   |          3.4 µs |       293,900 | (  2.4 µs …   5.5 µs) |   3.6 µs |   5.5 µs |   5.5 µs |

summary
  ts-pattern swc plugin
     1.00x slower than ts-pattern raw

group p-instanceof-patterns
| ts-pattern raw          |          3.1 µs |       323,400 | (  2.2 µs …   6.0 µs) |   3.3 µs |   6.0 µs |   6.0 µs |
| ts-pattern swc plugin   |         53.1 ns |    18,820,000 | ( 25.1 ns … 490.8 ns) |  51.1 ns | 201.7 ns | 273.2 ns |

summary
  ts-pattern swc plugin
    58.19x faster than ts-pattern raw

group p-intersection-patterns
| ts-pattern raw          |          5.1 µs |       196,000 | (  4.2 µs …   7.0 µs) |   5.9 µs |   7.0 µs |   7.0 µs |
| ts-pattern swc plugin   |         74.8 ns |    13,370,000 | ( 33.2 ns … 489.1 ns) |  73.7 ns | 237.5 ns | 284.3 ns |

summary
  ts-pattern swc plugin
    68.24x faster than ts-pattern raw

group p-map-patterns
| ts-pattern raw          |          3.9 µs |       256,100 | (  2.8 µs …   5.6 µs) |   4.4 µs |   5.6 µs |   5.6 µs |
| ts-pattern swc plugin   |         66.5 ns |    15,040,000 | ( 27.4 ns …   1.1 µs) |  62.4 ns | 342.4 ns | 424.7 ns |

summary
  ts-pattern swc plugin
    58.74x faster than ts-pattern raw

group p-narrow
| ts-pattern raw          |        745.7 ns |     1,341,000 | (493.3 ns …   1.4 µs) | 818.2 ns |   1.4 µs |   1.4 µs |
| ts-pattern swc plugin   |        220.8 ns |     4,528,000 | (124.3 ns … 533.7 ns) | 256.4 ns | 482.3 ns | 484.8 ns |

summary
  ts-pattern swc plugin
     3.38x faster than ts-pattern raw

group p-nonnullable-wildcard
| ts-pattern raw          |        186.1 ns |     5,375,000 | ( 94.8 ns … 514.7 ns) | 199.5 ns | 413.8 ns | 459.5 ns |
| ts-pattern swc plugin   |         24.9 ns |    40,200,000 | ( 11.4 ns … 321.7 ns) |  22.4 ns | 124.3 ns | 166.4 ns |

summary
  ts-pattern swc plugin
     7.48x faster than ts-pattern raw

group p-not-patterns
| ts-pattern raw          |          1.1 µs |       896,300 | (659.1 ns …   1.7 µs) |   1.2 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |         19.7 ns |    50,830,000 | ( 11.4 ns … 136.8 ns) |  22.0 ns |  42.6 ns |  53.8 ns |

summary
  ts-pattern swc plugin
    56.71x faster than ts-pattern raw

group p-nullish-wildcard
| ts-pattern raw          |        270.8 ns |     3,693,000 | (154.7 ns … 500.8 ns) | 299.5 ns | 481.4 ns | 500.8 ns |
| ts-pattern swc plugin   |         20.8 ns |    48,050,000 | ( 11.2 ns … 351.7 ns) |  22.5 ns |  84.8 ns | 162.0 ns |

summary
  ts-pattern swc plugin
    13.01x faster than ts-pattern raw

group p-number-between
| ts-pattern raw          |          1.7 µs |       601,000 | (  1.2 µs …   2.4 µs) |   1.8 µs |   2.4 µs |   2.4 µs |
| ts-pattern swc plugin   |         24.3 ns |    41,110,000 | ( 12.5 ns … 190.1 ns) |  24.1 ns |  92.8 ns | 128.4 ns |

summary
  ts-pattern swc plugin
    68.41x faster than ts-pattern raw

group p-number-finite
| ts-pattern raw          |          2.2 µs |       460,700 | (  1.4 µs …   4.7 µs) |   2.4 µs |   4.7 µs |   4.7 µs |
| ts-pattern swc plugin   |         32.3 ns |    30,950,000 | ( 11.6 ns … 915.3 ns) |  23.3 ns | 310.2 ns | 389.7 ns |

summary
  ts-pattern swc plugin
    67.18x faster than ts-pattern raw

group p-number-gt
| ts-pattern raw          |          2.0 µs |       503,100 | (  1.2 µs …   2.9 µs) |   2.1 µs |   2.9 µs |   2.9 µs |
| ts-pattern swc plugin   |         20.8 ns |    48,080,000 | ( 12.0 ns … 206.7 ns) |  22.9 ns |  54.7 ns |  86.3 ns |

summary
  ts-pattern swc plugin
    95.56x faster than ts-pattern raw

group p-number-gte
| ts-pattern raw          |          1.8 µs |       558,000 | (  1.2 µs …   2.3 µs) |   2.0 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         17.6 ns |    56,900,000 | ( 11.9 ns … 153.7 ns) |  22.1 ns |  32.9 ns |  38.6 ns |

summary
  ts-pattern swc plugin
   102.00x faster than ts-pattern raw

group p-number-int
| ts-pattern raw          |          1.6 µs |       640,500 | (  1.1 µs …   2.4 µs) |   1.7 µs |   2.4 µs |   2.4 µs |
| ts-pattern swc plugin   |         20.3 ns |    49,350,000 | ( 11.7 ns … 526.3 ns) |  22.9 ns |  39.4 ns |  48.7 ns |

summary
  ts-pattern swc plugin
    77.04x faster than ts-pattern raw

group p-number-lt
| ts-pattern raw          |          1.6 µs |       610,200 | (  1.1 µs …   3.3 µs) |   1.9 µs |   3.3 µs |   3.3 µs |
| ts-pattern swc plugin   |         19.4 ns |    51,540,000 | ( 12.0 ns … 405.3 ns) |  22.5 ns |  42.1 ns |  65.4 ns |

summary
  ts-pattern swc plugin
    84.47x faster than ts-pattern raw

group p-number-lte
| ts-pattern raw          |          1.7 µs |       586,200 | (  1.2 µs …   2.3 µs) |   1.9 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         21.4 ns |    46,790,000 | ( 11.8 ns … 431.7 ns) |  23.0 ns |  61.9 ns |  87.3 ns |

summary
  ts-pattern swc plugin
    79.83x faster than ts-pattern raw

group p-number-negative
| ts-pattern raw          |          1.9 µs |       522,000 | (  1.1 µs …   2.6 µs) |   2.1 µs |   2.6 µs |   2.6 µs |
| ts-pattern swc plugin   |         20.8 ns |    47,970,000 | ( 11.5 ns … 310.4 ns) |  22.3 ns |  83.9 ns | 128.2 ns |

summary
  ts-pattern swc plugin
    91.90x faster than ts-pattern raw

group p-number-positive
| ts-pattern raw          |          1.6 µs |       606,200 | (  1.1 µs …   2.3 µs) |   1.8 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         17.8 ns |    56,070,000 | ( 11.6 ns … 310.6 ns) |  21.9 ns |  24.7 ns |  25.7 ns |

summary
  ts-pattern swc plugin
    92.50x faster than ts-pattern raw

group p-number-wildcard
| ts-pattern raw          |        248.6 ns |     4,022,000 | (139.0 ns … 795.6 ns) | 279.6 ns | 544.9 ns | 643.9 ns |
| ts-pattern swc plugin   |         15.1 ns |    66,280,000 | ( 11.5 ns …  90.9 ns) |  18.1 ns |  24.8 ns |  25.3 ns |

summary
  ts-pattern swc plugin
    16.48x faster than ts-pattern raw

group p-optional-patterns
| ts-pattern raw          |          1.8 µs |       557,500 | (  1.4 µs …   3.0 µs) |   1.9 µs |   3.0 µs |   3.0 µs |
| ts-pattern swc plugin   |         39.3 ns |    25,410,000 | ( 24.6 ns … 110.3 ns) |  46.8 ns |  72.0 ns |  83.6 ns |

summary
  ts-pattern swc plugin
    45.58x faster than ts-pattern raw

group p-pattern
| ts-pattern raw          |        373.7 ns |     2,676,000 | (252.3 ns … 600.4 ns) | 424.7 ns | 575.5 ns | 600.4 ns |
| ts-pattern swc plugin   |        373.3 ns |     2,679,000 | (252.9 ns … 668.8 ns) | 427.1 ns | 593.7 ns | 668.8 ns |

summary
  ts-pattern swc plugin
     1.00x faster than ts-pattern raw

group p-record-profiles
| ts-pattern raw          |          2.1 µs |       469,900 | (  1.5 µs …   2.9 µs) |   2.4 µs |   2.9 µs |   2.9 µs |
| ts-pattern swc plugin   |        327.8 ns |     3,050,000 | (204.0 ns … 775.4 ns) | 372.8 ns | 525.7 ns | 775.4 ns |

summary
  ts-pattern swc plugin
     6.49x faster than ts-pattern raw

group p-record-scores
| ts-pattern raw          |          2.3 µs |       435,600 | (  1.7 µs …   3.1 µs) |   2.5 µs |   3.1 µs |   3.1 µs |
| ts-pattern swc plugin   |        495.6 ns |     2,018,000 | (344.5 ns … 719.3 ns) | 573.6 ns | 716.5 ns | 719.3 ns |

summary
  ts-pattern swc plugin
     4.63x faster than ts-pattern raw

group p-record-select
| ts-pattern raw          |          8.5 µs |       117,500 | (  2.7 µs …   7.5 ms) |   9.8 µs |  21.2 µs |  25.2 µs |
| ts-pattern swc plugin   |          1.3 µs |       768,900 | (828.2 ns …   1.7 µs) |   1.4 µs |   1.7 µs |   1.7 µs |

summary
  ts-pattern swc plugin
     6.54x faster than ts-pattern raw

group p-select-anonymous
| ts-pattern raw          |          1.6 µs |       612,500 | (  1.2 µs …   2.3 µs) |   1.7 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         47.1 ns |    21,220,000 | ( 34.3 ns … 122.5 ns) |  55.5 ns |  81.7 ns |  88.7 ns |

summary
  ts-pattern swc plugin
    34.64x faster than ts-pattern raw

group p-select-guarded
| ts-pattern raw          |          6.4 µs |       156,800 | (  4.5 µs …   8.5 µs) |   7.4 µs |   8.5 µs |   8.5 µs |
| ts-pattern swc plugin   |         62.7 ns |    15,950,000 | ( 38.1 ns … 364.7 ns) |  74.0 ns | 176.7 ns | 219.9 ns |

summary
  ts-pattern swc plugin
   101.70x faster than ts-pattern raw

group p-select-named
| ts-pattern raw          |          2.4 µs |       423,000 | (  1.8 µs …   4.2 µs) |   2.5 µs |   4.2 µs |   4.2 µs |
| ts-pattern swc plugin   |        101.0 ns |     9,902,000 | ( 64.4 ns … 443.3 ns) | 119.7 ns | 186.1 ns | 248.0 ns |

summary
  ts-pattern swc plugin
    23.41x faster than ts-pattern raw

group p-set-patterns
| ts-pattern raw          |          2.9 µs |       340,100 | (  2.4 µs …   5.8 µs) |   3.0 µs |   5.8 µs |   5.8 µs |
| ts-pattern swc plugin   |         43.5 ns |    23,010,000 | ( 22.5 ns … 732.5 ns) |  45.0 ns | 138.4 ns | 231.9 ns |

summary
  ts-pattern swc plugin
    67.65x faster than ts-pattern raw

group p-string-endswith
| ts-pattern raw          |          1.7 µs |       573,200 | (  1.1 µs …   3.7 µs) |   1.9 µs |   3.7 µs |   3.7 µs |
| ts-pattern swc plugin   |         20.9 ns |    47,880,000 | ( 12.7 ns … 390.3 ns) |  24.1 ns |  59.0 ns |  82.5 ns |

summary
  ts-pattern swc plugin
    83.53x faster than ts-pattern raw

group p-string-includes
| ts-pattern raw          |          1.2 µs |       806,200 | (933.2 ns …   1.7 µs) |   1.4 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |         19.0 ns |    52,660,000 | ( 14.7 ns …  32.7 ns) |  21.4 ns |  30.2 ns |  30.7 ns |

summary
  ts-pattern swc plugin
    65.32x faster than ts-pattern raw

group p-string-length
| ts-pattern raw          |          1.2 µs |       809,600 | (923.7 ns …   2.5 µs) |   1.3 µs |   2.5 µs |   2.5 µs |
| ts-pattern swc plugin   |         15.2 ns |    65,730,000 | ( 12.2 ns …  46.7 ns) |  16.6 ns |  24.7 ns |  25.2 ns |

summary
  ts-pattern swc plugin
    81.18x faster than ts-pattern raw

group p-string-maxlength
| ts-pattern raw          |          1.3 µs |       783,200 | (952.8 ns …   1.8 µs) |   1.4 µs |   1.8 µs |   1.8 µs |
| ts-pattern swc plugin   |         17.3 ns |    57,860,000 | ( 12.5 ns … 194.1 ns) |  20.9 ns |  29.4 ns |  35.6 ns |

summary
  ts-pattern swc plugin
    73.88x faster than ts-pattern raw

group p-string-minlength
| ts-pattern raw          |          1.3 µs |       781,300 | (981.3 ns …   1.6 µs) |   1.4 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         16.0 ns |    62,440,000 | ( 12.6 ns …  40.5 ns) |  18.5 ns |  28.8 ns |  31.0 ns |

summary
  ts-pattern swc plugin
    79.92x faster than ts-pattern raw

group p-string-regex
| ts-pattern raw          |          1.3 µs |       786,700 | (986.5 ns …   2.2 µs) |   1.3 µs |   2.2 µs |   2.2 µs |
| ts-pattern swc plugin   |         34.2 ns |    29,240,000 | ( 24.4 ns … 133.0 ns) |  40.1 ns |  68.9 ns |  77.1 ns |

summary
  ts-pattern swc plugin
    37.17x faster than ts-pattern raw

group p-string-startswith
| ts-pattern raw          |          1.2 µs |       818,300 | (958.6 ns …   1.8 µs) |   1.3 µs |   1.8 µs |   1.8 µs |
| ts-pattern swc plugin   |         15.3 ns |    65,280,000 | ( 12.6 ns … 100.7 ns) |  16.2 ns |  25.8 ns |  28.1 ns |

summary
  ts-pattern swc plugin
    79.77x faster than ts-pattern raw

group p-string-wildcard
| ts-pattern raw          |        232.8 ns |     4,295,000 | (133.8 ns … 639.2 ns) | 269.2 ns | 485.2 ns | 513.6 ns |
| ts-pattern swc plugin   |         18.1 ns |    55,260,000 | ( 13.1 ns … 102.2 ns) |  22.4 ns |  30.2 ns |  34.7 ns |

summary
  ts-pattern swc plugin
    12.87x faster than ts-pattern raw

group p-symbol-wildcard
| ts-pattern raw          |        129.4 ns |     7,726,000 | ( 75.5 ns … 267.4 ns) | 152.7 ns | 227.2 ns | 260.2 ns |
| ts-pattern swc plugin   |         16.0 ns |    62,630,000 | ( 12.7 ns …  37.3 ns) |  18.2 ns |  24.4 ns |  24.7 ns |

summary
  ts-pattern swc plugin
     8.11x faster than ts-pattern raw

group p-union-patterns
| ts-pattern raw          |          1.2 µs |       829,400 | (961.2 ns …   1.5 µs) |   1.3 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         40.4 ns |    24,770,000 | ( 32.7 ns …  66.2 ns) |  43.3 ns |  62.7 ns |  63.3 ns |

summary
  ts-pattern swc plugin
    29.87x faster than ts-pattern raw

group p-when-patterns
| ts-pattern raw          |          1.7 µs |       591,700 | (  1.3 µs …   2.2 µs) |   2.0 µs |   2.2 µs |   2.2 µs |
| ts-pattern swc plugin   |        137.5 ns |     7,274,000 | ( 98.1 ns … 257.6 ns) | 156.8 ns | 229.9 ns | 249.8 ns |

summary
  ts-pattern swc plugin
    12.29x faster than ts-pattern raw

group p-wildcard
| ts-pattern raw          |        168.5 ns |     5,936,000 | ( 95.9 ns …   1.1 µs) | 189.4 ns | 332.2 ns | 446.0 ns |
| ts-pattern swc plugin   |         14.7 ns |    68,090,000 | ( 11.1 ns … 190.5 ns) |  17.8 ns |  25.7 ns |  29.6 ns |

summary
  ts-pattern swc plugin
    11.47x faster than ts-pattern raw

group patterns-p
| ts-pattern raw          |        426.0 ns |     2,347,000 | (219.3 ns … 968.9 ns) | 509.6 ns | 958.0 ns | 968.9 ns |
| ts-pattern swc plugin   |         27.7 ns |    36,070,000 | ( 19.3 ns … 267.8 ns) |  32.5 ns |  52.9 ns |  67.4 ns |

summary
  ts-pattern swc plugin
    15.37x faster than ts-pattern raw

group patterns-pattern
| ts-pattern raw          |        440.3 ns |     2,271,000 | (264.0 ns … 726.9 ns) | 486.9 ns | 691.9 ns | 726.9 ns |
| ts-pattern swc plugin   |         25.9 ns |    38,580,000 | ( 19.1 ns … 133.1 ns) |  31.3 ns |  42.7 ns |  50.4 ns |

summary
  ts-pattern swc plugin
    16.99x faster than ts-pattern raw

group state-reducer
| ts-pattern raw          |          4.1 µs |       246,500 | (  3.2 µs …   5.0 µs) |   4.4 µs |   5.0 µs |   5.0 µs |
| ts-pattern swc plugin   |         93.4 ns |    10,710,000 | ( 69.4 ns … 216.6 ns) | 106.5 ns | 167.6 ns | 170.9 ns |

summary
  ts-pattern swc plugin
    43.44x faster than ts-pattern raw

group tuples-arrays
| ts-pattern raw          |          1.0 µs |       969,200 | (811.5 ns …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         56.7 ns |    17,630,000 | ( 41.6 ns … 174.6 ns) |  63.3 ns | 117.4 ns | 122.7 ns |

summary
  ts-pattern swc plugin
    18.19x faster than ts-pattern raw

group type-guard-function
| ts-pattern raw          |          1.2 µs |       850,300 | (966.2 ns …   1.5 µs) |   1.3 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         30.1 ns |    33,230,000 | ( 23.0 ns … 173.1 ns) |  34.0 ns |  53.1 ns |  59.7 ns |

summary
  ts-pattern swc plugin
    39.08x faster than ts-pattern raw

group type-narrowing-select
| ts-pattern raw          |          3.3 µs |       306,900 | (  2.8 µs …   4.0 µs) |   3.4 µs |   4.0 µs |   4.0 µs |
| ts-pattern swc plugin   |        259.0 ns |     3,861,000 | (208.1 ns … 482.7 ns) | 281.0 ns | 401.2 ns | 401.5 ns |

summary
  ts-pattern swc plugin
    12.58x faster than ts-pattern raw

group type-narrowing
| ts-pattern raw          |        902.7 ns |     1,108,000 | (590.5 ns …   1.2 µs) |   1.0 µs |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         26.7 ns |    37,410,000 | ( 20.4 ns …  68.0 ns) |  30.2 ns |  43.0 ns |  47.0 ns |

summary
  ts-pattern swc plugin
    33.77x faster than ts-pattern raw
```

<!-- /automd -->

## Supported input

The transformer handles chains that start from `match(value)` imported from `ts-pattern` and end in `.otherwise(handler)` or `.exhaustive()`.

Supported patterns:

- primitive literals: string, number, bigint, boolean, `null`, `undefined`
- object and array literal patterns
- `P._`
- `P.string`, `P.number`, `P.boolean`, `P.bigint`, `P.symbol`, `P.nullish`, `P.nonNullable`
- `P.optional(pattern)`, `P.not(pattern)`, `P.union(...patterns)`, `P.array(pattern?)`, `P.instanceOf(Ctor)`, `P.when(predicate)`

Supported selection patterns include `P.select` and array/record selection aggregation. The plugin also handles `.when`, `P.set`, `P.map`, `P.record`, `P.intersection`, and common string/number refinements.

Unsupported chains, custom matcher objects, `.returnType`, and unsupported combinations are left unchanged to preserve runtime behavior. Keep ts-pattern installed even when most expressions compile.
