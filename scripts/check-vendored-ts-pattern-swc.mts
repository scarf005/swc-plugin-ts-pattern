import { spawnSync } from "node:child_process"
import { cp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { transformWithSwcWasm } from "../web/src/swc-wasm-transform.ts"

const vendorRoot = new URL("../vendor/ts-pattern/", import.meta.url)
const testsRoot = new URL("tests/", vendorRoot)
const generatedRoot = new URL(".ts-pattern-swc-tests/", vendorRoot)
const generatedConfig = new URL("jest.config.cjs", generatedRoot)

const rewriteImports = (source: string) =>
  source
    .replaceAll("from '../src';", 'from "ts-pattern";')
    .replaceAll('from "../src";', 'from "ts-pattern";')
    .replaceAll("from '../src/index';", 'from "ts-pattern";')
    .replaceAll('from "../src/index";', 'from "ts-pattern";')

const fallbacksBaseline = new URL("vendored-fallbacks.json", import.meta.url)
const updateBaseline = process.argv.includes("--update-fallbacks")

// Counts the matches that stay on the runtime, by reason code, as the
// plugin reports them with `onFallback: "error"`.
const fallbackReasons = async (code: string) => {
  try {
    await transformWithSwcWasm({
      code,
      moduleType: "commonjs",
      plugin: true,
      pluginOptions: { onFallback: "error" },
    })
    return {}
  } catch (error) {
    const reasons: Record<string, number> = {}
    for (const [, reason] of String(error).matchAll(
      /stays on the ts-pattern runtime \(([\w-]+)\)/g,
    )) {
      reasons[reason] = (reasons[reason] ?? 0) + 1
    }
    if (Object.keys(reasons).length === 0) throw error
    return reasons
  }
}

const testFiles = async () => {
  const files = (await readdir(testsRoot, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.endsWith(".test.ts"))
    .map((entry) => entry.name)
    .sort()
  if (files.length === 0) throw new Error("No vendored ts-pattern tests found")
  return files
}

const writeGeneratedConfig = async () =>
  await writeFile(
    generatedConfig,
    `module.exports = {
  preset: 'ts-jest',
  testEnvironment: 'node',
  rootDir: '..',
  testMatch: ['<rootDir>/.ts-pattern-swc-tests/**/*.test.cjs'],
  moduleNameMapper: { '^ts-pattern$': '<rootDir>/src/index.ts' },
};
`,
  )

const runGeneratedTests = () => {
  const { status } = spawnSync(
    "npm",
    ["exec", "--", "jest", "--config", fileURLToPath(generatedConfig)],
    { cwd: vendorRoot, stdio: "inherit" },
  )
  if (status !== 0) throw new Error(`Generated Jest tests failed: ${status}`)
}

await rm(generatedRoot, { recursive: true, force: true })
await mkdir(generatedRoot, { recursive: true })
await cp(new URL("types-catalog/", testsRoot), new URL("types-catalog/", generatedRoot), {
  recursive: true,
})

try {
  const files = await testFiles()
  const failures: string[] = []
  const fallbacks: Record<string, Record<string, number>> = {}

  for (const file of files) {
    const url = new URL(file, testsRoot)
    const source = rewriteImports(await readFile(url, "utf8"))
    try {
      const output = await transformWithSwcWasm({
        code: source,
        moduleType: "commonjs",
        plugin: true,
      })
      const reasons = await fallbackReasons(source)
      if (Object.keys(reasons).length > 0) fallbacks[file] = reasons
      await writeFile(new URL(file.replace(/\.test\.ts$/, ".test.cjs"), generatedRoot), output.code)
    } catch (error) {
      failures.push(`${file}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  const baseline = JSON.parse(await readFile(fallbacksBaseline, "utf8"))
  if (updateBaseline) {
    await writeFile(fallbacksBaseline, `${JSON.stringify(fallbacks, null, 2)}\n`)
  } else if (JSON.stringify(fallbacks) !== JSON.stringify(baseline)) {
    failures.push(
      `Runtime fallbacks differ from scripts/vendored-fallbacks.json. Run \`just update-vendored-fallbacks\` after you check them:\n${JSON.stringify(fallbacks, null, 2)}`,
    )
  }

  if (failures.length > 0) {
    throw new Error(`Vendored ts-pattern SWC transform failed:\n${failures.join("\n")}`)
  }

  await writeGeneratedConfig()
  console.log(`Transformed ${files.length} vendored ts-pattern test files`)
  const total = Object.values(fallbacks)
    .flatMap(Object.values)
    .reduce((sum, count) => sum + count, 0)
  console.log(`${total} vendored ts-pattern matches stay on the runtime, as the baseline expects`)
  runGeneratedTests()
} finally {
  await rm(generatedRoot, { recursive: true, force: true })
}
