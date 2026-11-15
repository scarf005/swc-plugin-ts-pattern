import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { test } from "node:test"
import { promisify } from "node:util"
import { transformTsPattern } from "../transform.mjs"

const execute = promisify(execFile)

// A pattern from a parameter is unknown at compile time.
const source = `import { match } from "ts-pattern"
export const run = (input: unknown, pattern: unknown) =>
  match(input).with(pattern, () => 1).otherwise(() => 2)
`

// SWC drops plugin warnings, so the plugin prints them to stderr.
const transformInChild = async (pluginOptions) =>
  await execute(process.execPath, [
    "--input-type=module",
    "-e",
    `import { transformTsPattern } from ${JSON.stringify(new URL("../transform.mjs", import.meta.url).href)}
const { code } = await transformTsPattern(${JSON.stringify(source)}, { filename: "fallback.ts", sourceMaps: false, pluginOptions: ${JSON.stringify(pluginOptions)} })
process.stdout.write(code)`,
  ])

test('onFallback "warn" prints the file, line, column and reason of each runtime match', async () => {
  const { stdout, stderr } = await transformInChild({ onFallback: "warn" })
  assert.match(stdout, /\.with\(pattern/)
  assert.equal(
    stderr,
    "fallback.ts:3:3: warning: swc-plugin-ts-pattern: this match stays on the ts-pattern runtime (unsupported-pattern)\n",
  )
})

test("without onFallback the plugin prints nothing", async () => {
  const { stderr } = await transformInChild({})
  assert.equal(stderr, "")
})

test('onFallback "error" fails the transform at the runtime match', async () => {
  await assert.rejects(
    transformTsPattern(source, { filename: "fallback.ts", pluginOptions: { onFallback: "error" } }),
    /stays on the ts-pattern runtime \(unsupported-pattern\)\n\s*,-\[fallback\.ts:3:1\][^]*\n\s*:   \^{12}\n/,
  )
})

test('onFallback "error" accepts a file whose matches all compile', async () => {
  const { code } = await transformTsPattern(
    `import { match } from "ts-pattern"
export const run = (input: unknown) => match(input).with(1, () => 1).otherwise(() => 2)
`,
    { filename: "compiled.ts", pluginOptions: { onFallback: "error" } },
  )
  assert.doesNotMatch(code, /\.with\(/)
})

test("an unknown onFallback value fails the transform", async () => {
  await assert.rejects(
    transformTsPattern(source, { filename: "fallback.ts", pluginOptions: { onFallback: "log" } }),
    /onFallback must be "warn" or "error"/,
  )
})
