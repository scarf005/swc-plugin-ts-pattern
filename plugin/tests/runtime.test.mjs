import assert from "node:assert/strict"
import { execFileSync, spawnSync } from "node:child_process"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import { transformTsPattern } from "../transform.mjs"

const pluginRoot = fileURLToPath(new URL("../", import.meta.url))
const cli = fileURLToPath(new URL("../run.mjs", import.meta.url))
const register = fileURLToPath(new URL("../register.mjs", import.meta.url))
const denoAvailable = spawnSync("deno", ["--version"]).status === 0
// Node.js calls the resolve hook for `require` in a CommonJS file from 24.18.0 and 26.2.0.
// These temporary entries are CommonJS: the nearest package.json is the plugin's.
const [major, minor] = process.versions.node.split(".").map(Number)
const commonjsResolveSkip =
  major > 26 || (major === 26 && minor >= 2) || (major === 24 && minor >= 18)
    ? false
    : "this Node.js does not call resolve hooks for require() in a CommonJS file"

const withEntry = async (run) => {
  const directory = await mkdtemp(`${pluginRoot}tests/runtime-`)
  try {
    const entry = `${directory}/entry.ts`
    await writeFile(
      entry,
      `
import { match } from "ts-pattern";
const classify = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");
console.log(JSON.stringify({
  result: classify(1),
  compiled: !classify.toString().includes(".with("),
  args: typeof Deno === "undefined" ? process.argv.slice(2) : Deno.args,
}));
`,
    )
    await run(entry, directory)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

test(
  "public loader and transform declarations compose with Node synchronous hooks",
  { skip: !denoAvailable },
  () =>
    withEntry(async (entry) => {
      await writeFile(
        entry,
        `
import { registerHooks } from "node:module";
import { load, resolve } from ${JSON.stringify(new URL("../loader.mjs", import.meta.url).href)};
import { defaultPluginPath, transformTsPattern, transformTsPatternSync } from ${JSON.stringify(
          new URL("../transform.mjs", import.meta.url).href,
        )};
registerHooks({ load, resolve });
const path: string = await defaultPluginPath();
const asyncCode: string = (await transformTsPattern("", { pluginPath: path })).code;
const syncCode: string = transformTsPatternSync("", { pluginPath: path }).code;
`,
      )
      execFileSync("deno", ["check", "--config", `${pluginRoot}deno.json`, entry], {
        encoding: "utf8",
      })
    }),
)

for (const mode of ["cli", "preload"]) {
  test(`Node ${mode} executes transformed TypeScript and forwards arguments`, () =>
    withEntry(async (entry) => {
      const args = mode === "cli" ? [cli, entry] : ["--import", register, entry]
      const output = execFileSync(process.execPath, [...args, "first", "--flag"], {
        encoding: "utf8",
      })
      assert.deepEqual(JSON.parse(output), {
        result: "one",
        compiled: true,
        args: ["first", "--flag"],
      })
    }))

  test(
    `Deno ${mode} executes transformed TypeScript and forwards arguments`,
    {
      skip: !denoAvailable,
    },
    () =>
      withEntry(async (entry, directory) => {
        const config = `${directory}/deno.json`
        await writeFile(
          config,
          JSON.stringify({
            imports: {
              "ts-pattern": "npm:ts-pattern@5.9.0",
              "@swc/core": "npm:@swc/core@~1.15.0",
            },
            nodeModulesDir: "auto",
          }),
        )
        const args = mode === "cli" ? [cli, entry] : ["--preload", register, entry]
        const output = execFileSync(
          "deno",
          ["run", "-A", "--config", config, ...args, "first", "--flag"],
          { encoding: "utf8" },
        )
        assert.deepEqual(JSON.parse(output), {
          result: "one",
          compiled: true,
          args: ["first", "--flag"],
        })
      }),
  )
}

test("transform API respects parser, module and source map options", async () => {
  const result = await transformTsPattern(
    'import { match } from "ts-pattern"; export const value: number = match(1).with(1, () => 2).otherwise(() => 0)',
    {
      filename: "entry.cts",
      sourceMaps: false,
    },
  )
  assert.match(result.code, /exports/)
  assert.doesNotMatch(result.code, /\.with\(/)
  assert.doesNotMatch(result.code, /sourceMappingURL/)
  const mapped = await transformTsPattern("export const value: number = 1", {
    sourceMaps: true,
  })
  assert.equal(JSON.parse(mapped.map).version, 3)
})

test("transform API rejects invalid TypeScript and missing WASM", async () => {
  await assert.rejects(transformTsPattern("const ="))
  await assert.rejects(
    transformTsPattern("const value = 1", {
      pluginPath: `${pluginRoot}missing.wasm`,
    }),
    { code: "ENOENT" },
  )
})

for (const runtime of ["Node", "Deno"]) {
  for (const extension of ["ts", "tsx", "mts", "cts"]) {
    test(
      `${runtime} loader executes .${extension} modules with ${
        runtime === "Node" ? "extensionless" : "explicit"
      } TypeScript imports`,
      { skip: runtime === "Deno" ? !denoAvailable : commonjsResolveSkip },
      () =>
        withEntry(async (entry, directory) => {
          const module = `${directory}/child.${extension}`
          await writeFile(
            module,
            'import { match } from "ts-pattern"; export const classify = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");',
          )
          const specifier = runtime === "Node" ? "./child" : `./child.${extension}`
          const importClause =
            runtime === "Deno" && extension === "cts"
              ? `import child from "${specifier}"; const { classify } = child;`
              : `import { classify } from "${specifier}";`
          await writeFile(
            entry,
            `${importClause} console.log(JSON.stringify({ result: classify(1), compiled: !classify.toString().includes(".with(") }));`,
          )
          const config = `${directory}/deno.json`
          await writeFile(
            config,
            JSON.stringify({
              imports: {
                "ts-pattern": "npm:ts-pattern@5.9.0",
                "@swc/core": "npm:@swc/core@~1.15.0",
              },
              nodeModulesDir: "auto",
            }),
          )
          const command = runtime === "Node" ? process.execPath : "deno"
          const args =
            runtime === "Node"
              ? ["--import", register, entry]
              : ["run", "-A", "--config", config, "--preload", register, entry]
          const output = execFileSync(command, args, { encoding: "utf8" })
          assert.deepEqual(JSON.parse(output), {
            result: "one",
            compiled: true,
          })
        }),
    )
  }
}

const child =
  'import { match } from "ts-pattern"; export const classify = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");'
const report =
  'console.log(JSON.stringify({ result: classify(1), compiled: !classify.toString().includes(".with("), commonjs: typeof module !== "undefined" }));'

for (const type of ["module", "commonjs"]) {
  const skip = type === "commonjs" && commonjsResolveSkip
  test(`Node loader runs .ts as ${type} in a "type": "${type}" package`, { skip }, () =>
    withEntry(async (entry, directory) => {
      await writeFile(`${directory}/package.json`, JSON.stringify({ type }))
      await writeFile(`${directory}/child.ts`, child)
      await writeFile(
        entry,
        `${
          type === "module"
            ? 'import { classify } from "./child";'
            : 'const { classify } = require("./child");'
        } ${report}`,
      )
      const output = execFileSync(process.execPath, ["--import", register, entry], {
        encoding: "utf8",
      })
      assert.deepEqual(JSON.parse(output), {
        result: "one",
        compiled: true,
        commonjs: type === "commonjs",
      })
    }),
  )
}

// In a CommonJS file the real file name loads on every Node.js. A mapped name needs the resolve hook.
for (const [specifier, skip] of [
  ["./child.ts", false],
  ["./child.js", commonjsResolveSkip],
]) {
  test(`Node loader resolves ${specifier} to child.ts in a CommonJS file`, { skip }, () =>
    withEntry(async (entry, directory) => {
      await writeFile(`${directory}/child.ts`, child)
      await writeFile(entry, `const { classify } = require("${specifier}"); ${report}`)
      const output = execFileSync(process.execPath, ["--import", register, entry], {
        encoding: "utf8",
      })
      assert.deepEqual(JSON.parse(output), { result: "one", compiled: true, commonjs: true })
    }),
  )
}

for (const [specifier, extension] of [
  ["js", "ts"],
  ["js", "tsx"],
  ["jsx", "tsx"],
  ["mjs", "mts"],
  ["cjs", "cts"],
]) {
  test(`Node loader resolves ./child.${specifier} to child.${extension}`, () =>
    withEntry(async (entry, directory) => {
      await writeFile(`${directory}/package.json`, '{"type":"module"}')
      await writeFile(`${directory}/child.${extension}`, child)
      await writeFile(entry, `import { classify } from "./child.${specifier}"; ${report}`)
      const output = execFileSync(process.execPath, ["--import", register, entry], {
        encoding: "utf8",
      })
      assert.deepEqual(JSON.parse(output), {
        result: "one",
        compiled: true,
        commonjs: false,
      })
    }))
}

for (const [args, status] of [
  [[], 1],
  [["--help"], 0],
  [["--missing-option"], 1],
  [["missing-entry.ts"], 1],
]) {
  test(`CLI ${JSON.stringify(args)} exits ${status}`, () => {
    const result = spawnSync(process.execPath, [cli, ...args], {
      encoding: "utf8",
    })
    assert.equal(result.status, status, result.stderr)
  })
}
