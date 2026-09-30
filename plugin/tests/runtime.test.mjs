import assert from "node:assert/strict"
import { execFile, execFileSync, spawnSync } from "node:child_process"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:http"
import { promisify } from "node:util"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import { transformTsPattern } from "../transform.mjs"

const pluginRoot = fileURLToPath(new URL("../", import.meta.url))
const cli = fileURLToPath(new URL("../run.mjs", import.meta.url))
const register = fileURLToPath(new URL("../register.mjs", import.meta.url))
const denoAvailable = spawnSync("deno", ["--version"]).status === 0
const execFileAsync = promisify(execFile)

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
import { load, resolve } from ${
          JSON.stringify(new URL("../loader.mjs", import.meta.url).href)
        };
import { defaultPluginPath, transformTsPattern, transformTsPatternSync } from ${
          JSON.stringify(new URL("../transform.mjs", import.meta.url).href)
        };
registerHooks({ load, resolve });
const path: string = await defaultPluginPath();
const asyncCode: string = (await transformTsPattern("", { pluginPath: path })).code;
const syncCode: string = transformTsPatternSync("", { pluginPath: path }).code;
`,
      )
      execFileSync("deno", [
        "check",
        "--config",
        `${pluginRoot}deno.json`,
        entry,
      ], { encoding: "utf8" })
    }),
)

for (const mode of ["cli", "preload"]) {
  test(`Node ${mode} executes transformed TypeScript and forwards arguments`, () =>
    withEntry(async (entry) => {
      const args = mode === "cli" ? [cli, entry] : ["--import", register, entry]
      const output = execFileSync(process.execPath, [
        ...args,
        "first",
        "--flag",
      ], { encoding: "utf8" })
      assert.deepEqual(JSON.parse(output), {
        result: "one",
        compiled: true,
        args: ["first", "--flag"],
      })
    }))

  test(`Deno ${mode} executes transformed TypeScript and forwards arguments`, {
    skip: !denoAvailable,
  }, () =>
    withEntry(async (entry, directory) => {
      const config = `${directory}/deno.json`
      await writeFile(
        config,
        JSON.stringify({
          imports: {
            "ts-pattern": "npm:ts-pattern@5.9.0",
            "@swc/core": "npm:@swc/core@1.15.33",
          },
          nodeModulesDir: "auto",
        }),
      )
      const args = mode === "cli"
        ? [cli, entry]
        : ["--preload", register, entry]
      const output = execFileSync("deno", [
        "run",
        "-A",
        "--config",
        config,
        ...args,
        "first",
        "--flag",
      ], { encoding: "utf8" })
      assert.deepEqual(JSON.parse(output), {
        result: "one",
        compiled: true,
        args: ["first", "--flag"],
      })
    }))
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
      { skip: runtime === "Deno" && !denoAvailable },
      () =>
        withEntry(async (entry, directory) => {
          const module = `${directory}/child.${extension}`
          await writeFile(
            module,
            'import { match } from "ts-pattern"; export const classify = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");',
          )
          const specifier = runtime === "Node"
            ? "./child"
            : `./child.${extension}`
          const importClause = runtime === "Deno" && extension === "cts"
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
                "@swc/core": "npm:@swc/core@1.15.33",
              },
              nodeModulesDir: "auto",
            }),
          )
          const command = runtime === "Node" ? process.execPath : "deno"
          const args = runtime === "Node"
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

test(
  "Deno remote package materializes its exact WASM and transforms the local entry",
  { skip: !denoAvailable },
  () =>
    withEntry(async (entry, directory) => {
      const paths = new Set([
        "/run.mjs",
        "/register.mjs",
        "/loader.mjs",
        "/transform.mjs",
        "/target/wasm32-wasip1/release/ts_pattern_swc_plugin.wasm.bin",
      ])
      let wasmRequests = 0
      const server = createServer(async (request, response) => {
        if (!paths.has(request.url)) {
          response.writeHead(404).end()
          return
        }
        if (request.url.endsWith(".wasm.bin")) {
          wasmRequests += 1
          if (wasmRequests === 1) {
            response.writeHead(503).end("Unavailable")
            return
          }
          if (wasmRequests === 2) {
            response.end("Not WASM")
            return
          }
          if (wasmRequests === 3) {
            response.setHeader("Content-Length", 64 * 1024 * 1024 + 1)
            response.end("Too large")
            return
          }
          if (wasmRequests === 4) {
            const chunk = new Uint8Array(1024 * 1024)
            for (let index = 0; index < 64; index += 1) response.write(chunk)
            response.end(new Uint8Array(1))
            return
          }
        }
        response.setHeader(
          "Content-Type",
          request.url.endsWith(".mjs")
            ? "application/javascript"
            : "application/wasm",
        )
        response.end(await readFile(`${pluginRoot}${request.url.slice(1)}`))
      })
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
      try {
        const config = `${directory}/deno.json`
        await writeFile(
          config,
          JSON.stringify({
            imports: {
              "ts-pattern": "npm:ts-pattern@5.9.0",
              "@swc/core": "npm:@swc/core@1.15.33",
            },
            nodeModulesDir: "auto",
          }),
        )
        const base = `http://127.0.0.1:${server.address().port}`
        const consumer = `${directory}/consumer.ts`
        const temporaryRoot = `${directory}/cache`
        const executionOptions = {
          encoding: "utf8",
          env: {
            ...process.env,
            TMPDIR: temporaryRoot,
            TMP: temporaryRoot,
            TEMP: temporaryRoot,
          },
        }
        await writeFile(
          consumer,
          `
import assert from "node:assert/strict";
import { transformTsPattern, transformTsPatternSync, defaultPluginPath } from "${base}/transform.mjs";
const source = 'import { match } from "ts-pattern"; export const value = match(1).with(1, () => 2).otherwise(() => 0)';
assert.throws(() => transformTsPatternSync(source), /Resolve the remote WASM/);
await assert.rejects(transformTsPattern(source), /Unable to download plugin WASM: 503/);
await assert.rejects(transformTsPattern(source), /Invalid plugin WASM/);
await assert.rejects(transformTsPattern(source), /Plugin WASM exceeds 64 MiB/);
await assert.rejects(transformTsPattern(source), /Plugin WASM exceeds 64 MiB/);
const [first, second] = await Promise.all([transformTsPattern(source), transformTsPattern(source)]);
assert.ok(!first.code.includes(".with("));
assert.equal(first.code, second.code);
assert.ok(!transformTsPatternSync(source, { pluginPath: await defaultPluginPath() }).code.includes(".with("));
console.log(JSON.stringify(await defaultPluginPath()));
`,
        )
        const { stdout: cachedPath } = await execFileAsync(
          "deno",
          ["run", "-A", "--config", config, consumer],
          executionOptions,
        )
        await writeFile(JSON.parse(cachedPath), "corrupt cache fixture")
        assert.equal(
          wasmRequests,
          5,
          "failed downloads retry; concurrent transforms share one successful download",
        )
        for (
          const args of [["--preload", `${base}/register.mjs`, entry], [
            `${base}/run.mjs`,
            entry,
          ]]
        ) {
          const { stdout } = await execFileAsync("deno", [
            "run",
            "-A",
            "--config",
            config,
            ...args,
            "argument",
          ], executionOptions)
          assert.deepEqual(JSON.parse(stdout), {
            result: "one",
            compiled: true,
            args: ["argument"],
          })
        }
      } finally {
        await new Promise((resolve, reject) =>
          server.close((error) => error ? reject(error) : resolve())
        )
      }
    }),
)

for (
  const [args, status] of [
    [[], 1],
    [["--help"], 0],
    [["--missing-option"], 1],
    [["missing-entry.ts"], 1],
  ]
) {
  test(`CLI ${JSON.stringify(args)} exits ${status}`, () => {
    const result = spawnSync(process.execPath, [cli, ...args], {
      encoding: "utf8",
    })
    assert.equal(result.status, status, result.stderr)
  })
}
