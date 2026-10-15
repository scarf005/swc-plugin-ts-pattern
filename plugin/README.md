# @scarf005/swc-plugin-ts-pattern

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
npm install @scarf005/swc-plugin-ts-pattern
```

Or from JSR:

```sh
npx jsr add @scarf005/swc-plugin-ts-pattern
```

## Configure SWC

Use `@swc/core@1.15.33`, the runtime matched to this plugin's WASM ABI. Resolve the WASM explicitly rather than using the package root: the JSR/npm bridge root exports the transform API, while native npm archives use a WASM root.

```ts
import { transform } from "@swc/core";
import { defaultPluginPath } from "@scarf005/swc-plugin-ts-pattern/transform";

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
npm install ts-pattern @scarf005/swc-plugin-ts-pattern
```

Preload the loader:

```sh
node --import @scarf005/swc-plugin-ts-pattern/register ./foo.ts
```

### Deno

Use `import { match } from "ts-pattern"` with a `deno.json` import map, or `import { match } from "npm:ts-pattern@5.9.0"`. Run through the CLI:

```sh
deno run -A jsr:@scarf005/swc-plugin-ts-pattern/run ./foo.ts
```

The CLI imports the entry as a module. Use preload when your entry relies on native main-module identity (`import.meta.main` or `require.main`).

```sh
deno run -A --preload jsr:@scarf005/swc-plugin-ts-pattern/register ./foo.ts
```

Native JSR imports download the package's WASM into a content-addressed local cache before registering the loader; downloads are limited to 64 MiB, damaged cache files are replaced atomically, and download or transform failures abort execution. This requires network, filesystem and native SWC permissions.

Deno requires explicit extensions for relative imports. Import generated `.cts` modules through their default export, then destructure the exported members; Deno cannot discover the SWC-generated named CommonJS exports during static analysis.

For repository-local development, build with `cd plugin && npm ci && npm run build`, then run from the repository root using the examples import map:

```sh
deno run -A --config examples/deno.jsonc --preload ./plugin/register.mjs ./examples/literals.ts
```

### Programmatic transform

```ts
import { transformTsPattern } from "@scarf005/swc-plugin-ts-pattern/transform";

const result = await transformTsPattern(source, { filename: "foo.ts" });
console.log(result.code);
```

`defaultPluginPath()` is asynchronous so it can materialize a remote JSR WASM. `transformTsPattern()` prepares it automatically. `transformTsPatternSync()` accepts the same options; for remote imports, first await `defaultPluginPath()` or pass an explicit `pluginPath`.

Set `SWC_PLUGIN_TS_PATTERN_PATH` to use a custom-built WASM with the loader. For programmatic transforms, pass `pluginPath`. Restart the process after rebuilding a custom WASM: SWC caches loaded plugins for the process lifetime.

## Benchmark

The `bench` task compares [`bdbaraban/ts-pattern-benchmark`](https://github.com/bdbaraban/ts-pattern-benchmark) cases across raw `ts-pattern`, native code, and plugin output, then runs the local examples. The results below come from the [`Benchmark` workflow](https://github.com/scarf005/swc-plugin-ts-pattern/actions/workflows/bench.yml) on a GitHub-hosted runner; `just update-readme-bench-ci` copies the run for the current commit into this section.

<!-- automd:file src="../docs/just-bench.txt" code lang="text" -->

```text [just-bench.txt]
CPU | AMD EPYC 7763 64-Core Processor
Runtime | Deno 2.9.7 (x86_64-unknown-linux-gnu)

web/ts-pattern_bench.ts

| benchmark               | time/iter (avg) |        iter/s |      (min … max)      |      p75 |      p99 |     p995 |
| ----------------------- | --------------- | ------------- | --------------------- | -------- | -------- | -------- |

group ts-pattern-benchmark/always-last-digit
| ts-pattern raw          |        803.7 ns |     1,244,000 | (768.6 ns …   1.4 µs) | 791.3 ns |   1.4 µs |   1.4 µs |
| native code             |         15.8 ns |    63,130,000 | ( 15.5 ns …  31.8 ns) |  15.6 ns |  18.9 ns |  20.3 ns |
| ts-pattern swc plugin   |         17.0 ns |    58,850,000 | ( 16.7 ns …  25.7 ns) |  17.0 ns |  18.0 ns |  19.0 ns |

summary
  native code
     1.07x faster than ts-pattern swc plugin
    50.74x faster than ts-pattern raw

group ts-pattern-benchmark/random-digit
| ts-pattern raw          |        530.4 ns |     1,885,000 | (513.4 ns … 977.7 ns) | 524.9 ns | 916.1 ns | 977.7 ns |
| native code             |         16.2 ns |    61,580,000 | ( 15.8 ns …  22.6 ns) |  16.2 ns |  17.2 ns |  18.5 ns |
| ts-pattern swc plugin   |         18.2 ns |    54,840,000 | ( 17.7 ns …  36.9 ns) |  18.2 ns |  19.2 ns |  20.0 ns |

summary
  native code
     1.12x faster than ts-pattern swc plugin
    32.67x faster than ts-pattern raw

group ts-pattern-benchmark/nested-objects
| ts-pattern raw          |          2.2 µs |       464,300 | (  2.1 µs …   2.6 µs) |   2.1 µs |   2.6 µs |   2.6 µs |
| native code             |         20.1 ns |    49,730,000 | ( 19.4 ns …  28.1 ns) |  20.2 ns |  21.6 ns |  23.6 ns |
| ts-pattern swc plugin   |         17.7 ns |    56,520,000 | ( 17.5 ns …  25.4 ns) |  17.6 ns |  18.6 ns |  19.7 ns |

summary
  native code
     1.14x slower than ts-pattern swc plugin
   107.10x faster than ts-pattern raw

group intro-html
| ts-pattern raw          |          1.9 µs |       526,700 | (  1.9 µs …   2.3 µs) |   1.9 µs |   2.3 µs |   2.3 µs |
| ts-pattern swc plugin   |         66.5 ns |    15,030,000 | ( 64.3 ns … 104.1 ns) |  66.3 ns |  81.8 ns |  93.8 ns |

summary
  ts-pattern swc plugin
    28.54x faster than ts-pattern raw

group is-matching-curried
| ts-pattern raw          |        470.2 ns |     2,127,000 | (463.2 ns … 515.3 ns) | 476.3 ns | 511.2 ns | 515.3 ns |
| ts-pattern swc plugin   |         49.3 ns |    20,280,000 | ( 46.7 ns …  67.8 ns) |  49.8 ns |  56.2 ns |  60.4 ns |

summary
  ts-pattern swc plugin
     9.53x faster than ts-pattern raw

group is-matching-direct
| ts-pattern raw          |        481.1 ns |     2,079,000 | (451.9 ns … 795.3 ns) | 471.6 ns | 738.1 ns | 795.3 ns |
| ts-pattern swc plugin   |         37.1 ns |    26,950,000 | ( 34.6 ns …  80.0 ns) |  35.9 ns |  74.4 ns |  75.5 ns |

summary
  ts-pattern swc plugin
    12.97x faster than ts-pattern raw

group literals
| ts-pattern raw          |        408.4 ns |     2,448,000 | (377.0 ns … 755.4 ns) | 394.2 ns | 746.9 ns | 755.4 ns |
| ts-pattern swc plugin   |         22.8 ns |    43,790,000 | ( 22.1 ns …  49.5 ns) |  22.7 ns |  29.3 ns |  44.7 ns |

summary
  ts-pattern swc plugin
    17.89x faster than ts-pattern raw

group matching-several-patterns
| ts-pattern raw          |        448.4 ns |     2,230,000 | (435.4 ns … 973.5 ns) | 447.3 ns | 491.4 ns | 973.5 ns |
| ts-pattern swc plugin   |         28.4 ns |    35,270,000 | ( 27.4 ns …  36.8 ns) |  28.7 ns |  30.6 ns |  31.7 ns |

summary
  ts-pattern swc plugin
    15.82x faster than ts-pattern raw

group objects
| ts-pattern raw          |        831.6 ns |     1,202,000 | (804.5 ns …   1.2 µs) | 825.0 ns |   1.2 µs |   1.2 µs |
| ts-pattern swc plugin   |         47.8 ns |    20,920,000 | ( 46.1 ns …  69.3 ns) |  48.2 ns |  53.6 ns |  55.4 ns |

summary
  ts-pattern swc plugin
    17.40x faster than ts-pattern raw

group p-array-patterns
| ts-pattern raw          |          1.5 µs |       653,700 | (  1.5 µs …   1.7 µs) |   1.5 µs |   1.7 µs |   1.7 µs |
| ts-pattern swc plugin   |         34.3 ns |    29,140,000 | ( 33.3 ns …  53.3 ns) |  34.7 ns |  43.8 ns |  45.5 ns |

summary
  ts-pattern swc plugin
    44.57x faster than ts-pattern raw

group p-bigint-wildcard
| ts-pattern raw          |         90.1 ns |    11,100,000 | ( 86.2 ns … 122.0 ns) |  90.1 ns | 100.5 ns | 105.0 ns |
| ts-pattern swc plugin   |         15.3 ns |    65,300,000 | ( 15.0 ns …  27.6 ns) |  15.3 ns |  17.1 ns |  18.1 ns |

summary
  ts-pattern swc plugin
     5.88x faster than ts-pattern raw

group p-boolean-wildcard
| ts-pattern raw          |        205.0 ns |     4,877,000 | (197.8 ns … 239.1 ns) | 208.1 ns | 217.7 ns | 221.9 ns |
| ts-pattern swc plugin   |         14.8 ns |    67,650,000 | ( 14.5 ns …  30.7 ns) |  14.6 ns |  16.6 ns |  17.6 ns |

summary
  ts-pattern swc plugin
    13.87x faster than ts-pattern raw

group p-infer
| ts-pattern raw          |          1.9 µs |       531,200 | (  1.9 µs …   2.0 µs) |   1.9 µs |   2.0 µs |   2.0 µs |
| ts-pattern swc plugin   |         37.4 ns |    26,740,000 | ( 36.0 ns …  76.5 ns) |  37.5 ns |  53.6 ns |  74.0 ns |

summary
  ts-pattern swc plugin
    50.33x faster than ts-pattern raw

group p-instanceof-patterns
| ts-pattern raw          |          1.5 µs |       678,400 | (  1.5 µs …   1.6 µs) |   1.5 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         29.4 ns |    33,990,000 | ( 28.5 ns …  58.3 ns) |  29.7 ns |  31.5 ns |  33.1 ns |

summary
  ts-pattern swc plugin
    50.11x faster than ts-pattern raw

group p-intersection-patterns
| ts-pattern raw          |          2.8 µs |       351,800 | (  2.8 µs …   3.3 µs) |   2.8 µs |   3.3 µs |   3.3 µs |
| ts-pattern swc plugin   |         34.1 ns |    29,370,000 | ( 32.7 ns …  47.6 ns) |  34.4 ns |  37.2 ns |  39.9 ns |

summary
  ts-pattern swc plugin
    83.48x faster than ts-pattern raw

group p-map-patterns
| ts-pattern raw          |          2.2 µs |       465,000 | (  2.1 µs …   2.6 µs) |   2.1 µs |   2.6 µs |   2.6 µs |
| ts-pattern swc plugin   |         33.9 ns |    29,500,000 | ( 32.2 ns …  81.6 ns) |  33.8 ns |  44.3 ns |  45.8 ns |

summary
  ts-pattern swc plugin
    63.45x faster than ts-pattern raw

group p-narrow
| ts-pattern raw          |        523.9 ns |     1,909,000 | (510.1 ns … 600.9 ns) | 530.2 ns | 575.2 ns | 600.9 ns |
| ts-pattern swc plugin   |        146.4 ns |     6,831,000 | (143.3 ns … 198.8 ns) | 146.1 ns | 168.1 ns | 189.8 ns |

summary
  ts-pattern swc plugin
     3.58x faster than ts-pattern raw

group p-nonnullable-wildcard
| ts-pattern raw          |         93.2 ns |    10,720,000 | ( 88.4 ns … 135.1 ns) |  93.1 ns | 110.9 ns | 115.3 ns |
| ts-pattern swc plugin   |         14.9 ns |    66,940,000 | ( 14.2 ns …  27.3 ns) |  15.2 ns |  16.9 ns |  18.1 ns |

summary
  ts-pattern swc plugin
     6.24x faster than ts-pattern raw

group p-not-patterns
| ts-pattern raw          |        688.9 ns |     1,452,000 | (674.2 ns … 845.9 ns) | 690.0 ns | 845.9 ns | 845.9 ns |
| ts-pattern swc plugin   |         15.7 ns |    63,720,000 | ( 15.4 ns …  27.1 ns) |  15.6 ns |  16.6 ns |  17.6 ns |

summary
  ts-pattern swc plugin
    43.89x faster than ts-pattern raw

group p-nullish-wildcard
| ts-pattern raw          |        138.2 ns |     7,237,000 | (132.4 ns … 259.3 ns) | 141.0 ns | 158.9 ns | 175.9 ns |
| ts-pattern swc plugin   |         15.6 ns |    64,150,000 | ( 14.6 ns …  31.4 ns) |  15.8 ns |  18.1 ns |  21.9 ns |

summary
  ts-pattern swc plugin
     8.86x faster than ts-pattern raw

group p-number-between
| ts-pattern raw          |          1.2 µs |       847,200 | (  1.2 µs …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         17.0 ns |    58,790,000 | ( 16.3 ns …  31.4 ns) |  17.1 ns |  18.4 ns |  20.6 ns |

summary
  ts-pattern swc plugin
    69.38x faster than ts-pattern raw

group p-number-finite
| ts-pattern raw          |          1.2 µs |       857,200 | (  1.2 µs …   1.3 µs) |   1.2 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         14.7 ns |    67,970,000 | ( 14.5 ns …  23.0 ns) |  14.6 ns |  16.0 ns |  16.5 ns |

summary
  ts-pattern swc plugin
    79.29x faster than ts-pattern raw

group p-number-gt
| ts-pattern raw          |          1.2 µs |       855,000 | (  1.2 µs …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         15.6 ns |    63,970,000 | ( 14.9 ns …  27.4 ns) |  15.8 ns |  17.7 ns |  18.9 ns |

summary
  ts-pattern swc plugin
    74.82x faster than ts-pattern raw

group p-number-gte
| ts-pattern raw          |          1.2 µs |       840,200 | (  1.2 µs …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         16.1 ns |    62,280,000 | ( 15.6 ns …  26.4 ns) |  16.0 ns |  18.1 ns |  19.0 ns |

summary
  ts-pattern swc plugin
    74.13x faster than ts-pattern raw

group p-number-int
| ts-pattern raw          |          1.2 µs |       854,900 | (  1.2 µs …   1.3 µs) |   1.2 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         15.2 ns |    65,780,000 | ( 14.7 ns …  30.2 ns) |  15.2 ns |  16.2 ns |  16.9 ns |

summary
  ts-pattern swc plugin
    76.95x faster than ts-pattern raw

group p-number-lt
| ts-pattern raw          |          1.2 µs |       854,100 | (  1.1 µs …   1.4 µs) |   1.2 µs |   1.4 µs |   1.4 µs |
| ts-pattern swc plugin   |         15.7 ns |    63,750,000 | ( 15.2 ns …  31.9 ns) |  15.9 ns |  17.2 ns |  21.0 ns |

summary
  ts-pattern swc plugin
    74.63x faster than ts-pattern raw

group p-number-lte
| ts-pattern raw          |          1.2 µs |       860,500 | (  1.1 µs …   1.4 µs) |   1.2 µs |   1.4 µs |   1.4 µs |
| ts-pattern swc plugin   |         15.2 ns |    65,970,000 | ( 14.6 ns …  27.4 ns) |  15.6 ns |  17.1 ns |  17.7 ns |

summary
  ts-pattern swc plugin
    76.66x faster than ts-pattern raw

group p-number-negative
| ts-pattern raw          |          1.2 µs |       850,900 | (  1.2 µs …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         15.6 ns |    64,250,000 | ( 15.1 ns …  19.7 ns) |  15.6 ns |  17.1 ns |  17.4 ns |

summary
  ts-pattern swc plugin
    75.52x faster than ts-pattern raw

group p-number-positive
| ts-pattern raw          |          1.2 µs |       850,200 | (  1.2 µs …   1.3 µs) |   1.2 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         15.5 ns |    64,380,000 | ( 14.6 ns …  22.4 ns) |  15.7 ns |  16.8 ns |  17.6 ns |

summary
  ts-pattern swc plugin
    75.72x faster than ts-pattern raw

group p-number-wildcard
| ts-pattern raw          |        149.1 ns |     6,707,000 | (143.9 ns … 176.7 ns) | 152.6 ns | 159.7 ns | 169.0 ns |
| ts-pattern swc plugin   |         16.0 ns |    62,590,000 | ( 15.7 ns …  26.4 ns) |  15.9 ns |  17.7 ns |  18.2 ns |

summary
  ts-pattern swc plugin
     9.33x faster than ts-pattern raw

group p-optional-patterns
| ts-pattern raw          |          1.5 µs |       670,000 | (  1.5 µs …   1.6 µs) |   1.5 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         28.4 ns |    35,170,000 | ( 27.6 ns …  36.8 ns) |  28.8 ns |  30.4 ns |  31.5 ns |

summary
  ts-pattern swc plugin
    52.49x faster than ts-pattern raw

group p-pattern
| ts-pattern raw          |        326.2 ns |     3,065,000 | (322.0 ns … 351.1 ns) | 327.5 ns | 347.8 ns | 351.1 ns |
| ts-pattern swc plugin   |         24.0 ns |    41,700,000 | ( 23.2 ns …  59.2 ns) |  24.3 ns |  26.9 ns |  29.9 ns |

summary
  ts-pattern swc plugin
    13.60x faster than ts-pattern raw

group p-record-profiles
| ts-pattern raw          |          1.7 µs |       595,000 | (  1.6 µs …   3.0 µs) |   1.6 µs |   3.0 µs |   3.0 µs |
| ts-pattern swc plugin   |        234.2 ns |     4,269,000 | (230.6 ns … 262.6 ns) | 234.4 ns | 248.6 ns | 250.9 ns |

summary
  ts-pattern swc plugin
     7.17x faster than ts-pattern raw

group p-record-scores
| ts-pattern raw          |          1.9 µs |       526,800 | (  1.9 µs …   2.0 µs) |   1.9 µs |   2.0 µs |   2.0 µs |
| ts-pattern swc plugin   |        301.5 ns |     3,317,000 | (296.6 ns … 348.5 ns) | 303.2 ns | 324.8 ns | 348.5 ns |

summary
  ts-pattern swc plugin
     6.30x faster than ts-pattern raw

group p-record-select
| ts-pattern raw          |          5.3 µs |       188,500 | (  5.3 µs …   5.4 µs) |   5.3 µs |   5.4 µs |   5.4 µs |
| ts-pattern swc plugin   |        736.7 ns |     1,357,000 | (722.9 ns …   1.0 µs) | 739.3 ns |   1.0 µs |   1.0 µs |

summary
  ts-pattern swc plugin
     7.20x faster than ts-pattern raw

group p-select-anonymous
| ts-pattern raw          |          1.2 µs |       823,300 | (  1.2 µs …   1.5 µs) |   1.2 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         41.5 ns |    24,080,000 | ( 40.5 ns …  77.7 ns) |  41.8 ns |  43.3 ns |  44.2 ns |

summary
  ts-pattern swc plugin
    29.25x faster than ts-pattern raw

group p-select-guarded
| ts-pattern raw          |          4.7 µs |       212,800 | (  4.7 µs …   5.1 µs) |   4.7 µs |   5.1 µs |   5.1 µs |
| ts-pattern swc plugin   |         41.7 ns |    23,990,000 | ( 40.7 ns …  51.7 ns) |  42.0 ns |  45.6 ns |  46.7 ns |

summary
  ts-pattern swc plugin
   112.70x faster than ts-pattern raw

group p-select-named
| ts-pattern raw          |          1.8 µs |       567,200 | (  1.7 µs …   2.2 µs) |   1.8 µs |   2.2 µs |   2.2 µs |
| ts-pattern swc plugin   |         77.1 ns |    12,980,000 | ( 74.9 ns … 103.7 ns) |  77.0 ns |  93.9 ns |  94.5 ns |

summary
  ts-pattern swc plugin
    22.88x faster than ts-pattern raw

group p-set-patterns
| ts-pattern raw          |          1.7 µs |       583,500 | (  1.7 µs …   2.2 µs) |   1.7 µs |   2.2 µs |   2.2 µs |
| ts-pattern swc plugin   |         27.4 ns |    36,540,000 | ( 26.2 ns …  59.7 ns) |  27.6 ns |  36.9 ns |  37.5 ns |

summary
  ts-pattern swc plugin
    62.62x faster than ts-pattern raw

group p-string-endswith
| ts-pattern raw          |          1.1 µs |       878,600 | (  1.1 µs …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         16.4 ns |    60,840,000 | ( 15.4 ns …  37.2 ns) |  16.7 ns |  19.3 ns |  20.9 ns |

summary
  ts-pattern swc plugin
    69.25x faster than ts-pattern raw

group p-string-includes
| ts-pattern raw          |          1.1 µs |       875,500 | (  1.1 µs …   1.5 µs) |   1.1 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         21.3 ns |    47,000,000 | ( 19.8 ns …  27.0 ns) |  21.4 ns |  22.5 ns |  24.0 ns |

summary
  ts-pattern swc plugin
    53.68x faster than ts-pattern raw

group p-string-length
| ts-pattern raw          |          1.1 µs |       894,000 | (  1.1 µs …   1.5 µs) |   1.1 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         15.3 ns |    65,390,000 | ( 14.8 ns …  22.1 ns) |  15.7 ns |  16.6 ns |  17.0 ns |

summary
  ts-pattern swc plugin
    73.14x faster than ts-pattern raw

group p-string-maxlength
| ts-pattern raw          |          1.1 µs |       899,700 | (  1.1 µs …   1.5 µs) |   1.1 µs |   1.5 µs |   1.5 µs |
| ts-pattern swc plugin   |         16.0 ns |    62,670,000 | ( 15.2 ns …  26.1 ns) |  16.2 ns |  17.4 ns |  18.9 ns |

summary
  ts-pattern swc plugin
    69.65x faster than ts-pattern raw

group p-string-minlength
| ts-pattern raw          |          1.1 µs |       896,100 | (  1.1 µs …   1.3 µs) |   1.1 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         16.4 ns |    60,990,000 | ( 15.9 ns …  26.3 ns) |  16.4 ns |  17.3 ns |  17.6 ns |

summary
  ts-pattern swc plugin
    68.07x faster than ts-pattern raw

group p-string-regex
| ts-pattern raw          |          1.2 µs |       867,700 | (  1.1 µs …   1.3 µs) |   1.2 µs |   1.3 µs |   1.3 µs |
| ts-pattern swc plugin   |         29.1 ns |    34,330,000 | ( 28.2 ns …  52.5 ns) |  29.5 ns |  36.7 ns |  38.0 ns |

summary
  ts-pattern swc plugin
    39.57x faster than ts-pattern raw

group p-string-startswith
| ts-pattern raw          |          1.1 µs |       870,800 | (  1.1 µs …   2.1 µs) |   1.1 µs |   2.1 µs |   2.1 µs |
| ts-pattern swc plugin   |         16.8 ns |    59,690,000 | ( 16.5 ns …  35.1 ns) |  16.6 ns |  18.0 ns |  20.5 ns |

summary
  ts-pattern swc plugin
    68.55x faster than ts-pattern raw

group p-string-wildcard
| ts-pattern raw          |        159.7 ns |     6,263,000 | (154.5 ns … 299.9 ns) | 162.8 ns | 171.7 ns | 193.5 ns |
| ts-pattern swc plugin   |         16.6 ns |    60,340,000 | ( 16.1 ns …  32.2 ns) |  17.0 ns |  18.1 ns |  18.3 ns |

summary
  ts-pattern swc plugin
     9.63x faster than ts-pattern raw

group p-symbol-wildcard
| ts-pattern raw          |         87.1 ns |    11,490,000 | ( 83.4 ns … 105.0 ns) |  86.9 ns |  96.3 ns |  98.5 ns |
| ts-pattern swc plugin   |         15.9 ns |    63,080,000 | ( 15.2 ns …  22.2 ns) |  16.1 ns |  17.0 ns |  17.4 ns |

summary
  ts-pattern swc plugin
     5.49x faster than ts-pattern raw

group p-union-patterns
| ts-pattern raw          |          1.1 µs |       915,200 | (  1.1 µs …   1.1 µs) |   1.1 µs |   1.1 µs |   1.1 µs |
| ts-pattern swc plugin   |         37.0 ns |    27,010,000 | ( 36.1 ns …  44.8 ns) |  37.4 ns |  38.3 ns |  40.1 ns |

summary
  ts-pattern swc plugin
    29.52x faster than ts-pattern raw

group p-when-patterns
| ts-pattern raw          |          1.4 µs |       690,800 | (  1.4 µs …   1.6 µs) |   1.4 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |        116.0 ns |     8,622,000 | (114.3 ns … 141.8 ns) | 116.2 ns | 122.3 ns | 128.7 ns |

summary
  ts-pattern swc plugin
    12.48x faster than ts-pattern raw

group p-wildcard
| ts-pattern raw          |         95.0 ns |    10,530,000 | ( 89.2 ns … 138.5 ns) |  96.9 ns | 103.5 ns | 104.9 ns |
| ts-pattern swc plugin   |         13.7 ns |    72,850,000 | ( 13.3 ns …  21.3 ns) |  13.9 ns |  15.1 ns |  15.8 ns |

summary
  ts-pattern swc plugin
     6.92x faster than ts-pattern raw

group patterns-p
| ts-pattern raw          |        241.9 ns |     4,133,000 | (228.9 ns … 506.6 ns) | 245.0 ns | 279.5 ns | 391.7 ns |
| ts-pattern swc plugin   |         25.8 ns |    38,820,000 | ( 24.7 ns …  48.5 ns) |  26.2 ns |  28.8 ns |  31.2 ns |

summary
  ts-pattern swc plugin
     9.39x faster than ts-pattern raw

group patterns-pattern
| ts-pattern raw          |        248.5 ns |     4,024,000 | (237.3 ns … 506.6 ns) | 252.0 ns | 284.0 ns | 288.9 ns |
| ts-pattern swc plugin   |         25.0 ns |    40,060,000 | ( 24.0 ns …  46.9 ns) |  25.4 ns |  27.6 ns |  31.5 ns |

summary
  ts-pattern swc plugin
     9.96x faster than ts-pattern raw

group state-reducer
| ts-pattern raw          |          3.4 µs |       292,500 | (  3.3 µs …   4.5 µs) |   3.4 µs |   4.5 µs |   4.5 µs |
| ts-pattern swc plugin   |         85.0 ns |    11,760,000 | ( 82.8 ns … 108.7 ns) |  85.1 ns |  96.6 ns |  98.8 ns |

summary
  ts-pattern swc plugin
    40.21x faster than ts-pattern raw

group tuples-arrays
| ts-pattern raw          |        924.5 ns |     1,082,000 | (904.6 ns …   1.0 µs) | 922.3 ns |   1.0 µs |   1.0 µs |
| ts-pattern swc plugin   |         53.3 ns |    18,760,000 | ( 51.4 ns …  95.1 ns) |  53.7 ns |  57.2 ns |  63.0 ns |

summary
  ts-pattern swc plugin
    17.35x faster than ts-pattern raw

group type-guard-function
| ts-pattern raw          |          1.2 µs |       850,300 | (  1.2 µs …   1.6 µs) |   1.2 µs |   1.6 µs |   1.6 µs |
| ts-pattern swc plugin   |         26.6 ns |    37,600,000 | ( 25.6 ns …  50.5 ns) |  26.8 ns |  31.6 ns |  37.7 ns |

summary
  ts-pattern swc plugin
    44.22x faster than ts-pattern raw

group type-narrowing-select
| ts-pattern raw          |          2.9 µs |       339,100 | (  2.9 µs …   3.2 µs) |   3.0 µs |   3.2 µs |   3.2 µs |
| ts-pattern swc plugin   |        260.9 ns |     3,833,000 | (257.0 ns … 284.2 ns) | 261.8 ns | 279.2 ns | 280.2 ns |

summary
  ts-pattern swc plugin
    11.30x faster than ts-pattern raw

group type-narrowing
| ts-pattern raw          |        711.0 ns |     1,407,000 | (692.1 ns …   1.1 µs) | 707.3 ns |   1.1 µs |   1.1 µs |
| ts-pattern swc plugin   |         26.8 ns |    37,320,000 | ( 26.0 ns …  37.5 ns) |  27.2 ns |  29.1 ns |  30.6 ns |

summary
  ts-pattern swc plugin
    26.54x faster than ts-pattern raw
```

<!-- /automd -->

## Supported input

The transformer handles chains that start from `match(value)` imported from `ts-pattern` and end in `.otherwise(handler)` or `.exhaustive()`. It also compiles `isMatching(pattern)` and `isMatching(pattern, value)` when the pattern holds only literals and side-effect-free inline callbacks and `value` is an identifier or literal.

Supported patterns:

- primitive literals: string, number, bigint, boolean, `null`, `undefined`
- object and array literal patterns
- `P._`
- `P.string`, `P.number`, `P.boolean`, `P.bigint`, `P.symbol`, `P.nullish`, `P.nonNullable`
- `P.optional(pattern)`, `P.not(pattern)`, `P.union(...patterns)`, `P.array(pattern?)`, `P.instanceOf(Ctor)`, `P.when(predicate)`
- chained `.optional()`, `.and(pattern)`, and `.or(pattern)` on patterns that provide them

Supported selection patterns include `P.select` and array/record selection aggregation. Record key selections preserve numeric coercion and original-key matching. Selections named `__proto__`, and record selections using inherited `Object.prototype` names such as `constructor` or `toString`, retain the runtime to preserve its selection and error behavior. The plugin also handles `.when`, `P.set`, `P.map`, `P.record`, `P.intersection`, and common string/number refinements.

Unsupported chains, custom matcher objects, `.returnType`, and unsupported combinations are left unchanged to preserve runtime behavior. Keep ts-pattern installed even when most expressions compile.
