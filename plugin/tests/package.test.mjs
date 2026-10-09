import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"
import { test } from "node:test"

const execute = promisify(execFile)
const pluginRoot = fileURLToPath(new URL("../", import.meta.url))

test("packed npm archive exposes a working WASM, transform API, loader and CLI", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ts-pattern-package-test-"))
  try {
    const { stdout: packed } = await execute("npm", [
      "pack",
      "--ignore-scripts",
      "--json",
      "--pack-destination",
      directory,
    ], { cwd: pluginRoot })
    const [{ filename }] = JSON.parse(packed)
    await writeFile(
      join(directory, "package.json"),
      JSON.stringify({ private: true, type: "module" }),
    )
    await execute("npm", [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--package-lock=false",
      join(directory, filename),
    ], { cwd: directory })
    const env = { ...process.env }
    delete env.SWC_PLUGIN_TS_PATTERN_PATH
    const options = { cwd: directory, env }
    await writeFile(
      join(directory, "entry.ts"),
      `
import { match } from "ts-pattern";
const classify = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");
console.log(JSON.stringify({ result: classify(1), compiled: !classify.toString().includes(".with("), args: process.argv.slice(2) }));
`,
    )
    for (
      const args of [
        [
          "--import",
          "@scarf005/swc-plugin-ts-pattern/register",
          "entry.ts",
          "argument",
        ],
        ["node_modules/.bin/ts-pattern-swc", "entry.ts", "argument"],
      ]
    ) {
      const { stdout } = await execute(process.execPath, args, options)
      assert.deepEqual(JSON.parse(stdout), {
        result: "one",
        compiled: true,
        args: ["argument"],
      })
    }
    await writeFile(
      join(directory, "consumer.mjs"),
      `
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { transform } from "@swc/core";
import { defaultPluginPath, transformTsPattern } from "@scarf005/swc-plugin-ts-pattern/transform";
const require = createRequire(import.meta.url);
const wasm = require.resolve("@scarf005/swc-plugin-ts-pattern");
assert.equal(await defaultPluginPath(), wasm);
assert.equal((await readFile(wasm)).subarray(0, 4).toString(), "\\0asm");
const source = 'import { match } from "ts-pattern"; export const value = match(1).with(1, () => 2).otherwise(() => 0)';
const direct = await transform(source, { jsc: { experimental: { plugins: [[wasm, {}]] } } });
const api = await transformTsPattern(source);
assert.ok(!direct.code.includes(".with("));
assert.ok(!api.code.includes(".with("));
`,
    )
    await execute(process.execPath, ["consumer.mjs"], options)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})
