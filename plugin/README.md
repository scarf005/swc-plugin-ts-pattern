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
| ts-pattern raw          |        792.1 ns |     1,262,000 | (677.7 ns …   1.0 µs) | 844.3 ns |   1.0 µs |   1.0 µs |
| native code             |         13.3 ns |    75,210,000 | ( 11.9 ns …  25.4 ns) |  13.5 ns |  23.6 ns |  24.4 ns |
| ts-pattern swc plugin   |         14.9 ns |    67,330,000 | ( 12.8 ns …  37.5 ns) |  15.3 ns |  25.5 ns |  25.9 ns |

summary
  native code
     1.12x faster than ts-pattern swc plugin
    59.58x faster than ts-pattern raw

group ts-pattern-benchmark/random-digit
| ts-pattern raw          |        546.9 ns |     1,828,000 | (445.2 ns … 865.3 ns) | 584.4 ns | 710.0 ns | 865.3 ns |
| native code             |         13.6 ns |    73,320,000 | ( 11.2 ns …  64.0 ns) |  14.3 ns |  23.7 ns |  24.2 ns |
| ts-pattern swc plugin   |         15.1 ns |    66,040,000 | ( 12.1 ns …  27.2 ns) |  15.4 ns |  26.0 ns |  26.4 ns |

summary
  native code
     1.11x faster than ts-pattern swc plugin
    40.10x faster than ts-pattern raw

group ts-pattern-benchmark/nested-objects
| ts-pattern raw          |          1.9 µs |       518,700 | (  1.8 µs …   2.4 µs) |   2.0 µs |   2.4 µs |   2.4 µs |
| native code             |         17.8 ns |    56,150,000 | ( 15.3 ns …  31.9 ns) |  18.3 ns |  29.6 ns |  30.0 ns |
| ts-pattern swc plugin   |         15.4 ns |    64,740,000 | ( 13.2 ns … 113.8 ns) |  15.8 ns |  27.5 ns |  27.7 ns |

summary
  native code
     1.15x slower than ts-pattern swc plugin
   108.30x faster than ts-pattern raw

group intro-html
| ts-pattern raw          |          1.7 µs |       595,300 | (  1.5 µs …   2.1 µs) |   1.7 µs |   2.1 µs |   2.1 µs |
| ts-pattern swc plugin   |         61.1 ns |    16,360,000 | ( 54.0 ns … 110.6 ns) |  62.0 ns | 101.0 ns | 105.9 ns |

summary
  ts-pattern swc plugin
    27.49x faster than ts-pattern raw

group is-matching-curried
| ts-pattern raw          |        397.8 ns |     2,514,000 | (357.5 ns … 543.6 ns) | 413.6 ns | 542.6 ns | 543.6 ns |
| ts-pattern swc plugin   |        393.8 ns |     2,539,000 | (359.0 ns … 595.3 ns) | 398.5 ns | 559.6 ns | 595.3 ns |

summary
  ts-pattern swc plugin
     1.01x faster than ts-pattern raw

group is-matching-direct
| ts-pattern raw          |        380.2 ns |     2,630,000 | (337.6 ns … 547.0 ns) | 394.6 ns | 540.4 ns | 547.0 ns |
| ts-pattern swc plugin   |        374.9 ns |     2,668,000 | (337.3 ns … 501.0 ns) | 383.8 ns | 483.8 ns | 501.0 ns |

summary
  ts-pattern swc plugin
     1.01x faster than ts-pattern raw

group literals
| ts-pattern raw          |        396.8 ns |     2,520,000 | (330.3 ns … 535.6 ns) | 423.8 ns | 517.8 ns | 535.6 ns |
| ts-pattern swc plugin   |         18.5 ns |    54,060,000 | ( 16.2 ns …  47.0 ns) |  18.7 ns |  32.1 ns |  33.0 ns |

summary
  ts-pattern swc plugin
    21.45x faster than ts-pattern raw

group matching-several-patterns
| ts-pattern raw          |        392.7 ns |     2,546,000 | (339.6 ns … 675.5 ns) | 403.4 ns | 597.0 ns | 675.5 ns |
| ts-pattern swc plugin   |         22.9 ns |    43,690,000 | ( 20.0 ns …  41.0 ns) |  23.2 ns |  39.1 ns |  40.0 ns |

summary
  ts-pattern swc plugin
    17.16x faster than ts-pattern raw

group objects
| ts-pattern raw          |        720.6 ns |     1,388,000 | (645.5 ns … 885.0 ns) | 756.9 ns | 885.0 ns | 885.0 ns |
| ts-pattern swc plugin   |         42.1 ns |    23,750,000 | ( 35.6 ns …  78.5 ns) |  43.5 ns |  73.6 ns |  74.5 ns |

summary
  ts-pattern swc plugin
    17.11x faster than ts-pattern raw

group p-array-patterns
| ts-pattern raw          |          1.4 µs |       718,800 | (  1.3 µs …   1.6 µs) |   1.5 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         30.7 ns |    32,560,000 | ( 26.0 ns …  69.7 ns) |  31.7 ns |  52.3 ns |  53.6 ns |

summary
  ts-pattern swc plugin
    45.30x faster than ts-pattern raw

group p-bigint-wildcard
| ts-pattern raw          |         94.9 ns |    10,540,000 | ( 74.7 ns … 181.6 ns) | 103.2 ns | 146.6 ns | 152.2 ns |
| ts-pattern swc plugin   |         14.3 ns |    70,040,000 | ( 12.2 ns …  52.6 ns) |  14.7 ns |  23.2 ns |  23.6 ns |

summary
  ts-pattern swc plugin
     6.65x faster than ts-pattern raw

group p-boolean-wildcard
| ts-pattern raw          |        213.7 ns |     4,680,000 | (171.9 ns … 324.7 ns) | 232.6 ns | 313.5 ns | 320.2 ns |
| ts-pattern swc plugin   |         13.0 ns |    77,130,000 | ( 11.2 ns …  33.4 ns) |  13.2 ns |  22.8 ns |  23.1 ns |

summary
  ts-pattern swc plugin
    16.48x faster than ts-pattern raw

group p-infer
| ts-pattern raw          |          1.7 µs |       578,900 | (  1.6 µs …   2.0 µs) |   1.8 µs |   2.0 µs |   2.0 µs |
| ts-pattern swc plugin   |          1.7 µs |       576,700 | (  1.6 µs …   2.3 µs) |   1.8 µs |   2.3 µs |   2.3 µs |

summary
  ts-pattern swc plugin
     1.00x slower than ts-pattern raw

group p-instanceof-patterns
| ts-pattern raw          |          1.4 µs |       725,400 | (  1.2 µs …   1.7 µs) |   1.5 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |         27.5 ns |    36,390,000 | ( 24.1 ns …  70.2 ns) |  27.9 ns |  45.6 ns |  47.1 ns |

summary
  ts-pattern swc plugin
    50.16x faster than ts-pattern raw

group p-intersection-patterns
| ts-pattern raw          |          2.6 µs |       383,500 | (  2.4 µs …   2.8 µs) |   2.8 µs |   2.8 µs |   2.8 µs |
| ts-pattern swc plugin   |         38.0 ns |    26,290,000 | ( 32.0 ns …  76.2 ns) |  38.9 ns |  65.2 ns |  68.2 ns |

summary
  ts-pattern swc plugin
    68.54x faster than ts-pattern raw

group p-map-patterns
| ts-pattern raw          |          1.9 µs |       521,300 | (  1.8 µs …   2.3 µs) |   2.0 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         31.7 ns |    31,510,000 | ( 25.7 ns …  71.9 ns) |  33.1 ns |  54.8 ns |  60.1 ns |

summary
  ts-pattern swc plugin
    60.44x faster than ts-pattern raw

group p-narrow
| ts-pattern raw          |        475.8 ns |     2,102,000 | (413.1 ns … 648.2 ns) | 497.5 ns | 642.0 ns | 648.2 ns |
| ts-pattern swc plugin   |        137.6 ns |     7,268,000 | (120.8 ns … 215.4 ns) | 142.3 ns | 208.1 ns | 213.8 ns |

summary
  ts-pattern swc plugin
     3.46x faster than ts-pattern raw

group p-nonnullable-wildcard
| ts-pattern raw          |         97.7 ns |    10,230,000 | ( 76.8 ns … 164.2 ns) | 106.3 ns | 145.2 ns | 149.7 ns |
| ts-pattern swc plugin   |         12.8 ns |    78,230,000 | ( 11.0 ns …  33.6 ns) |  13.1 ns |  21.8 ns |  22.2 ns |

summary
  ts-pattern swc plugin
     7.64x faster than ts-pattern raw

group p-not-patterns
| ts-pattern raw          |        923.1 ns |     1,083,000 | (570.3 ns …   1.5 µs) |   1.0 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         17.3 ns |    57,800,000 | ( 11.2 ns … 258.8 ns) |  21.2 ns |  24.8 ns |  29.6 ns |

summary
  ts-pattern swc plugin
    53.35x faster than ts-pattern raw

group p-nullish-wildcard
| ts-pattern raw          |        231.4 ns |     4,321,000 | (123.5 ns … 626.4 ns) | 272.1 ns | 529.5 ns | 582.3 ns |
| ts-pattern swc plugin   |         13.2 ns |    75,880,000 | ( 11.8 ns …  24.1 ns) |  13.4 ns |  22.6 ns |  23.2 ns |

summary
  ts-pattern swc plugin
    17.56x faster than ts-pattern raw

group p-number-between
| ts-pattern raw          |          1.0 µs |       959,500 | (932.8 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         14.4 ns |    69,520,000 | ( 12.8 ns …  25.2 ns) |  14.5 ns |  23.5 ns |  23.9 ns |

summary
  ts-pattern swc plugin
    72.45x faster than ts-pattern raw

group p-number-finite
| ts-pattern raw          |          1.0 µs |       988,900 | (914.0 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         12.5 ns |    80,180,000 | ( 10.9 ns …  24.0 ns) |  12.5 ns |  21.7 ns |  22.2 ns |

summary
  ts-pattern swc plugin
    81.08x faster than ts-pattern raw

group p-number-gt
| ts-pattern raw          |          1.0 µs |       980,800 | (918.9 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         13.7 ns |    72,900,000 | ( 12.1 ns …  60.9 ns) |  14.0 ns |  22.6 ns |  23.2 ns |

summary
  ts-pattern swc plugin
    74.32x faster than ts-pattern raw

group p-number-gte
| ts-pattern raw          |          1.0 µs |       955,900 | (950.4 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         13.5 ns |    74,280,000 | ( 12.1 ns …  29.9 ns) |  13.5 ns |  22.8 ns |  23.3 ns |

summary
  ts-pattern swc plugin
    77.70x faster than ts-pattern raw

group p-number-int
| ts-pattern raw          |          1.0 µs |       997,800 | (919.1 ns …   1.2 µs) |   1.0 µs |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         13.2 ns |    75,630,000 | ( 11.5 ns …  37.4 ns) |  13.4 ns |  22.2 ns |  22.6 ns |

summary
  ts-pattern swc plugin
    75.80x faster than ts-pattern raw

group p-number-lt
| ts-pattern raw          |          1.0 µs |       986,000 | (935.9 ns …   1.2 µs) |   1.1 µs |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         13.5 ns |    74,160,000 | ( 11.6 ns …  27.7 ns) |  13.8 ns |  22.7 ns |  23.1 ns |

summary
  ts-pattern swc plugin
    75.22x faster than ts-pattern raw

group p-number-lte
| ts-pattern raw          |          1.1 µs |       930,000 | (951.9 ns …   1.4 µs) |   1.1 µs |   1.4 µs |   1.4 µs |
| ts-pattern swc plugin   |         13.0 ns |    76,720,000 | ( 11.3 ns …  55.2 ns) |  13.2 ns |  22.6 ns |  22.8 ns |

summary
  ts-pattern swc plugin
    82.49x faster than ts-pattern raw

group p-number-negative
| ts-pattern raw          |          1.0 µs |       953,800 | (929.2 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         12.8 ns |    78,350,000 | ( 11.4 ns …  24.8 ns) |  12.9 ns |  23.1 ns |  23.4 ns |

summary
  ts-pattern swc plugin
    82.14x faster than ts-pattern raw

group p-number-positive
| ts-pattern raw          |          1.0 µs |       959,700 | (941.3 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         13.3 ns |    75,390,000 | ( 11.4 ns …  24.9 ns) |  13.5 ns |  22.9 ns |  23.4 ns |

summary
  ts-pattern swc plugin
    78.56x faster than ts-pattern raw

group p-number-wildcard
| ts-pattern raw          |        165.6 ns |     6,038,000 | (128.5 ns … 260.9 ns) | 181.2 ns | 253.3 ns | 259.4 ns |
| ts-pattern swc plugin   |         14.2 ns |    70,260,000 | ( 11.3 ns …  42.3 ns) |  15.6 ns |  23.6 ns |  24.3 ns |

summary
  ts-pattern swc plugin
    11.64x faster than ts-pattern raw

group p-optional-patterns
| ts-pattern raw          |          1.4 µs |       698,300 | (  1.3 µs …   1.9 µs) |   1.5 µs |   1.9 µs |   1.9 µs |
| ts-pattern swc plugin   |         28.0 ns |    35,690,000 | ( 24.8 ns …  50.7 ns) |  27.9 ns |  48.6 ns |  49.2 ns |

summary
  ts-pattern swc plugin
    51.11x faster than ts-pattern raw

group p-pattern
| ts-pattern raw          |        271.5 ns |     3,684,000 | (239.1 ns … 453.4 ns) | 282.8 ns | 415.8 ns | 453.4 ns |
| ts-pattern swc plugin   |        268.0 ns |     3,731,000 | (240.7 ns … 505.6 ns) | 274.4 ns | 376.1 ns | 505.6 ns |

summary
  ts-pattern swc plugin
     1.01x faster than ts-pattern raw

group p-record-profiles
| ts-pattern raw          |          1.5 µs |       672,600 | (  1.3 µs …   1.7 µs) |   1.6 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |        208.0 ns |     4,808,000 | (186.9 ns … 300.2 ns) | 211.3 ns | 285.3 ns | 288.3 ns |

summary
  ts-pattern swc plugin
     7.15x faster than ts-pattern raw

group p-record-scores
| ts-pattern raw          |          1.7 µs |       583,000 | (  1.6 µs …   2.0 µs) |   1.8 µs |   2.0 µs |   2.0 µs |
| ts-pattern swc plugin   |        370.8 ns |     2,697,000 | (329.4 ns … 540.0 ns) | 391.4 ns | 514.4 ns | 540.0 ns |

summary
  ts-pattern swc plugin
     4.63x faster than ts-pattern raw

group p-record-select
| ts-pattern raw          |          4.8 µs |       209,400 | (  4.5 µs …   5.2 µs) |   4.9 µs |   5.2 µs |   5.2 µs |
| ts-pattern swc plugin   |        811.5 ns |     1,232,000 | (742.3 ns … 990.2 ns) | 833.2 ns | 990.2 ns | 990.2 ns |

summary
  ts-pattern swc plugin
     5.89x faster than ts-pattern raw

group p-select-anonymous
| ts-pattern raw          |          1.1 µs |       921,500 | (971.0 ns …   1.4 µs) |   1.1 µs |   1.4 µs |   1.4 µs |
| ts-pattern swc plugin   |         38.2 ns |    26,160,000 | ( 33.6 ns …  69.6 ns) |  38.5 ns |  66.1 ns |  66.7 ns |

summary
  ts-pattern swc plugin
    28.38x faster than ts-pattern raw

group p-select-guarded
| ts-pattern raw          |          4.2 µs |       237,700 | (  4.0 µs …   4.6 µs) |   4.3 µs |   4.6 µs |   4.6 µs |
| ts-pattern swc plugin   |         41.4 ns |    24,170,000 | ( 37.0 ns …  79.6 ns) |  41.7 ns |  70.5 ns |  74.1 ns |

summary
  ts-pattern swc plugin
   101.70x faster than ts-pattern raw

group p-select-named
| ts-pattern raw          |          1.6 µs |       637,000 | (  1.4 µs …   1.8 µs) |   1.6 µs |   1.8 µs |   1.8 µs |
| ts-pattern swc plugin   |         73.2 ns |    13,670,000 | ( 62.8 ns … 127.6 ns) |  76.8 ns | 116.0 ns | 123.2 ns |

summary
  ts-pattern swc plugin
    21.45x faster than ts-pattern raw

group p-set-patterns
| ts-pattern raw          |          1.5 µs |       664,600 | (  1.4 µs …   1.8 µs) |   1.6 µs |   1.8 µs |   1.8 µs |
| ts-pattern swc plugin   |         24.6 ns |    40,630,000 | ( 20.6 ns …  58.0 ns) |  25.0 ns |  43.3 ns |  45.3 ns |

summary
  ts-pattern swc plugin
    61.14x faster than ts-pattern raw

group p-string-endswith
| ts-pattern raw          |          1.0 µs |       995,100 | (915.7 ns …   1.2 µs) |   1.0 µs |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         14.1 ns |    70,980,000 | ( 12.0 ns …  37.6 ns) |  14.0 ns |  24.3 ns |  24.6 ns |

summary
  ts-pattern swc plugin
    71.33x faster than ts-pattern raw

group p-string-includes
| ts-pattern raw          |        996.1 ns |     1,004,000 | (893.8 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         17.3 ns |    57,670,000 | ( 15.2 ns …  31.7 ns) |  17.3 ns |  29.5 ns |  30.1 ns |

summary
  ts-pattern swc plugin
    57.45x faster than ts-pattern raw

group p-string-length
| ts-pattern raw          |          1.0 µs |       998,100 | (888.7 ns …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         14.2 ns |    70,660,000 | ( 12.1 ns …  24.6 ns) |  14.4 ns |  23.7 ns |  24.0 ns |

summary
  ts-pattern swc plugin
    70.79x faster than ts-pattern raw

group p-string-maxlength
| ts-pattern raw          |        977.8 ns |     1,023,000 | (878.3 ns …   1.2 µs) |   1.0 µs |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         13.8 ns |    72,500,000 | ( 11.7 ns …  33.7 ns) |  13.9 ns |  23.4 ns |  24.0 ns |

summary
  ts-pattern swc plugin
    70.89x faster than ts-pattern raw

group p-string-minlength
| ts-pattern raw          |        993.5 ns |     1,007,000 | (891.7 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         13.9 ns |    72,150,000 | ( 12.0 ns …  25.2 ns) |  14.0 ns |  23.4 ns |  23.9 ns |

summary
  ts-pattern swc plugin
    71.68x faster than ts-pattern raw

group p-string-regex
| ts-pattern raw          |          1.0 µs |       991,900 | (911.2 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         27.8 ns |    35,960,000 | ( 23.5 ns …  62.4 ns) |  28.4 ns |  46.0 ns |  52.2 ns |

summary
  ts-pattern swc plugin
    36.25x faster than ts-pattern raw

group p-string-startswith
| ts-pattern raw          |          1.0 µs |       993,700 | (890.5 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         13.4 ns |    74,860,000 | ( 11.8 ns …  33.2 ns) |  13.4 ns |  23.8 ns |  24.4 ns |

summary
  ts-pattern swc plugin
    75.34x faster than ts-pattern raw

group p-string-wildcard
| ts-pattern raw          |        152.9 ns |     6,540,000 | (128.2 ns … 248.7 ns) | 161.0 ns | 230.2 ns | 241.6 ns |
| ts-pattern swc plugin   |         14.6 ns |    68,610,000 | ( 12.1 ns …  35.0 ns) |  14.9 ns |  24.4 ns |  24.9 ns |

summary
  ts-pattern swc plugin
    10.49x faster than ts-pattern raw

group p-symbol-wildcard
| ts-pattern raw          |         90.9 ns |    11,010,000 | ( 73.3 ns … 145.6 ns) |  99.0 ns | 138.9 ns | 141.7 ns |
| ts-pattern swc plugin   |         14.0 ns |    71,270,000 | ( 12.4 ns …  24.7 ns) |  14.2 ns |  23.4 ns |  23.8 ns |

summary
  ts-pattern swc plugin
     6.47x faster than ts-pattern raw

group p-union-patterns
| ts-pattern raw          |          1.0 µs |       997,200 | (900.5 ns …   1.3 µs) |   1.0 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         33.8 ns |    29,550,000 | ( 29.5 ns …  88.3 ns) |  34.2 ns |  58.3 ns |  59.3 ns |

summary
  ts-pattern swc plugin
    29.63x faster than ts-pattern raw

group p-when-patterns
| ts-pattern raw          |          1.3 µs |       770,200 | (  1.2 µs …   1.6 µs) |   1.3 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |        109.5 ns |     9,134,000 | ( 96.5 ns … 203.6 ns) | 111.6 ns | 163.5 ns | 167.3 ns |

summary
  ts-pattern swc plugin
    11.86x faster than ts-pattern raw

group p-wildcard
| ts-pattern raw          |        100.5 ns |     9,947,000 | ( 79.0 ns … 175.7 ns) | 108.7 ns | 156.1 ns | 171.8 ns |
| ts-pattern swc plugin   |         11.7 ns |    85,750,000 | ( 10.3 ns …  22.2 ns) |  11.9 ns |  20.1 ns |  20.6 ns |

summary
  ts-pattern swc plugin
     8.62x faster than ts-pattern raw

group patterns-p
| ts-pattern raw          |        247.9 ns |     4,034,000 | (204.3 ns … 360.5 ns) | 265.8 ns | 357.8 ns | 358.7 ns |
| ts-pattern swc plugin   |         21.0 ns |    47,510,000 | ( 18.2 ns …  41.4 ns) |  21.2 ns |  35.9 ns |  37.6 ns |

summary
  ts-pattern swc plugin
    11.78x faster than ts-pattern raw

group patterns-pattern
| ts-pattern raw          |        251.9 ns |     3,970,000 | (208.7 ns … 363.8 ns) | 268.0 ns | 343.5 ns | 356.0 ns |
| ts-pattern swc plugin   |         20.8 ns |    48,170,000 | ( 18.3 ns …  39.3 ns) |  20.8 ns |  34.6 ns |  36.2 ns |

summary
  ts-pattern swc plugin
    12.13x faster than ts-pattern raw

group state-reducer
| ts-pattern raw          |          3.1 µs |       318,900 | (  2.8 µs …   4.2 µs) |   3.2 µs |   4.2 µs |   4.2 µs |
| ts-pattern swc plugin   |         81.1 ns |    12,330,000 | ( 67.7 ns … 147.9 ns) |  81.8 ns | 142.1 ns | 144.9 ns |

summary
  ts-pattern swc plugin
    38.66x faster than ts-pattern raw

group tuples-arrays
| ts-pattern raw          |        828.6 ns |     1,207,000 | (731.6 ns …   1.2 µs) | 856.3 ns |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         47.1 ns |    21,240,000 | ( 40.5 ns …  99.2 ns) |  46.6 ns |  84.2 ns |  85.2 ns |

summary
  ts-pattern swc plugin
    17.60x faster than ts-pattern raw

group type-guard-function
| ts-pattern raw          |          1.1 µs |       934,900 | (933.3 ns …   1.7 µs) |   1.1 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |         26.4 ns |    37,920,000 | ( 21.8 ns …  66.9 ns) |  27.4 ns |  45.1 ns |  46.9 ns |

summary
  ts-pattern swc plugin
    40.56x faster than ts-pattern raw

group type-narrowing-select
| ts-pattern raw          |          2.7 µs |       375,000 | (  2.4 µs …   3.3 µs) |   2.8 µs |   3.3 µs |   3.3 µs |
| ts-pattern swc plugin   |        218.3 ns |     4,580,000 | (203.1 ns … 309.5 ns) | 221.1 ns | 293.6 ns | 301.2 ns |

summary
  ts-pattern swc plugin
    12.21x faster than ts-pattern raw

group type-narrowing
| ts-pattern raw          |        592.4 ns |     1,688,000 | (537.6 ns … 830.3 ns) | 605.2 ns | 830.3 ns | 830.3 ns |
| ts-pattern swc plugin   |         22.6 ns |    44,320,000 | ( 20.4 ns …  42.4 ns) |  22.8 ns |  37.1 ns |  39.1 ns |

summary
  ts-pattern swc plugin
    26.25x faster than ts-pattern raw
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

Supported selection patterns include `P.select` and array/record selection aggregation. Record key selections preserve numeric coercion and original-key matching. Selections named `__proto__`, and record selections using inherited `Object.prototype` names such as `constructor` or `toString`, retain the runtime to preserve its selection and error behavior. The plugin also handles `.when`, `P.set`, `P.map`, `P.record`, `P.intersection`, and common string/number refinements.

Unsupported chains, custom matcher objects, `.returnType`, and unsupported combinations are left unchanged to preserve runtime behavior. Keep ts-pattern installed even when most expressions compile.
