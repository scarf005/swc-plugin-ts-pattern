import { spawnSync } from "node:child_process";
import { cp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { transformWithSwcWasm } from "../web/src/swc-wasm-transform.ts";

const vendorRoot = new URL("../vendor/ts-pattern/", import.meta.url);
const testsRoot = new URL("tests/", vendorRoot);
const generatedRoot = new URL(".ts-pattern-swc-tests/", vendorRoot);
const generatedConfig = new URL("jest.config.cjs", generatedRoot);

const rewriteImports = (source: string) =>
  source
    .replaceAll("from '../src';", 'from "ts-pattern";')
    .replaceAll('from "../src";', 'from "ts-pattern";')
    .replaceAll("from '../src/index';", 'from "ts-pattern";')
    .replaceAll('from "../src/index";', 'from "ts-pattern";');

const noMatchRequiredFiles = new Set([
  "instance-of.test.ts",
  "otherwise.test.ts",
  "strings.test.ts",
  "unions.test.ts",
]);

const escapeRegExp = (value: string) =>
  value.replace(/[\\^$.*+?()[\]{}|]/g, "\\$&");

const tsPatternRequireAliases = (source: string) =>
  [...source.matchAll(
    /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*require\(["'](?:npm:)?ts-pattern["']\);/g,
  )].map(([, alias]) => alias);

// The plugin keeps `throw match(input).exhaustive()` as the no-match fallback,
// so ts-pattern throws its own NonExhaustiveError.
const withoutExhaustiveFallbacks = (source: string) =>
  source.replace(
    /throw \(0, [A-Za-z_$][\w$]*\.match\)\([A-Za-z_$][\w$]*\)\.exhaustive\(\);/g,
    "",
  );

const residualTsPatternMatchCall = (code: string) => {
  const source = withoutExhaustiveFallbacks(code);
  const directMatch = /require\(["'](?:npm:)?ts-pattern["']\)\.match\s*\(/.exec(
    source,
  );
  if (directMatch) return directMatch[0];

  for (const alias of tsPatternRequireAliases(source)) {
    const aliasMatch = new RegExp(
      `(?:\\(0,\\s*)?${escapeRegExp(alias)}\\.match\\)?\\s*\\(`,
    ).exec(source);
    if (aliasMatch) return aliasMatch[0];
  }
};

const testFiles = async () => {
  const files = (await readdir(testsRoot, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.endsWith(".test.ts"))
    .map((entry) => entry.name)
    .sort();
  if (files.length === 0) throw new Error("No vendored ts-pattern tests found");
  return files;
};

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
  );

const runGeneratedTests = () => {
  const { status } = spawnSync(
    "npm",
    ["exec", "--", "jest", "--config", fileURLToPath(generatedConfig)],
    { cwd: vendorRoot, stdio: "inherit" },
  );
  if (status !== 0) throw new Error(`Generated Jest tests failed: ${status}`);
};

await rm(generatedRoot, { recursive: true, force: true });
await mkdir(generatedRoot, { recursive: true });
await cp(
  new URL("types-catalog/", testsRoot),
  new URL("types-catalog/", generatedRoot),
  { recursive: true },
);

try {
  const files = await testFiles();
  const filesRequiringNoMatch = files.filter((file) =>
    noMatchRequiredFiles.has(file)
  );
  const missingNoMatchFiles = [...noMatchRequiredFiles].filter((file) =>
    !files.includes(file)
  );
  const failures: string[] = missingNoMatchFiles.length > 0
    ? [
      `No-match requirement files not found: ${missingNoMatchFiles.join(", ")}`,
    ]
    : [];

  for (const file of files) {
    const url = new URL(file, testsRoot);
    const source = rewriteImports(await readFile(url, "utf8"));
    try {
      const output = await transformWithSwcWasm({
        code: source,
        moduleType: "commonjs",
        plugin: true,
      });
      const residualMatch = noMatchRequiredFiles.has(file)
        ? residualTsPatternMatchCall(output.code)
        : undefined;
      if (residualMatch) {
        failures.push(
          `${file}: transformed output still calls ts-pattern match: ${residualMatch}`,
        );
      }
      await writeFile(
        new URL(file.replace(/\.test\.ts$/, ".test.cjs"), generatedRoot),
        output.code,
      );
    } catch (error) {
      failures.push(
        `${file}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  if (failures.length > 0) {
    throw new Error(
      `Vendored ts-pattern SWC transform failed:\n${failures.join("\n")}`,
    );
  }

  await writeGeneratedConfig();
  console.log(`Transformed ${files.length} vendored ts-pattern test files`);
  console.log(
    `Verified ${filesRequiringNoMatch.length} vendored ts-pattern test files contain no runtime match calls`,
  );
  runGeneratedTests();
} finally {
  await rm(generatedRoot, { recursive: true, force: true });
}
