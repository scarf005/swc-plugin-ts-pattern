/// <reference lib="deno.ns" />

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
  source.replace(/throw \(0, [A-Za-z_$][\w$]*\.match\)\([A-Za-z_$][\w$]*\)\.exhaustive\(\);/g, "");

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
  const files: string[] = [];
  for await (const entry of Deno.readDir(testsRoot)) {
    if (entry.isFile && entry.name.endsWith(".test.ts")) files.push(entry.name);
  }
  files.sort();
  if (files.length === 0) throw new Error("No vendored ts-pattern tests found");
  return files;
};

const copyDirectory = async (from: URL, to: URL) => {
  await Deno.mkdir(to, { recursive: true });
  for await (const entry of Deno.readDir(from)) {
    const source = new URL(entry.name, from);
    const target = new URL(entry.name, to);
    if (entry.isDirectory) {
      await copyDirectory(
        new URL(`${entry.name}/`, from),
        new URL(`${entry.name}/`, to),
      );
    } else if (entry.isFile) {
      await Deno.copyFile(source, target);
    }
  }
};

const writeGeneratedConfig = async () =>
  await Deno.writeTextFile(
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

const runGeneratedTests = async () => {
  const command = new Deno.Command("npm", {
    args: [
      "exec",
      "--",
      "jest",
      "--config",
      Deno.realPathSync(generatedConfig),
    ],
    cwd: Deno.realPathSync(vendorRoot),
    stdout: "inherit",
    stderr: "inherit",
  });
  const status = await command.output();
  if (!status.success) {
    throw new Error(`Generated Jest tests failed: ${status.code}`);
  }
};

await Deno.remove(generatedRoot, { recursive: true }).catch((error) => {
  if (!(error instanceof Deno.errors.NotFound)) throw error;
});
await Deno.mkdir(generatedRoot, { recursive: true });
await copyDirectory(
  new URL("types-catalog/", testsRoot),
  new URL("types-catalog/", generatedRoot),
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
    const source = rewriteImports(await Deno.readTextFile(url));
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
      await Deno.writeTextFile(
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
  await runGeneratedTests();
} finally {
  await Deno.remove(generatedRoot, { recursive: true }).catch((error) => {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  });
}
