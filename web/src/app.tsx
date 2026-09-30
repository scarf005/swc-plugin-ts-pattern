/// <reference types="vite/client" />

import "./app.css"
import { signal } from "@preact/signals"
import { useEffect, useLayoutEffect, useRef } from "preact/hooks"
import * as monaco from "monaco-editor/editor/editor.api.js"
import {
  benchmarkModules,
  formatBenchmarkComparison,
  prepareBenchmarkInputs,
} from "./benchmark.ts"
import { type ModuleType, transformInBrowser } from "./browser-transform.ts"
import { cycleValue, getWheelStep } from "./example-wheel.ts"
import { garbageInputPairsForInputs, seedFromString } from "./property.ts"
import { formatValue, getModule } from "./runtime.ts"
import "monaco-editor/languages/definitions/javascript/register.js"
import "monaco-editor/languages/definitions/typescript/register.js"
import * as ts from "monaco-editor/languages/features/typescript/register.js"
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker"
import TsWorker from "monaco-editor/languages/features/typescript/ts.worker.js?worker"
const monacoGlobals = globalThis as typeof globalThis & {
  MonacoEnvironment?: monaco.Environment
}

monacoGlobals.MonacoEnvironment = {
  getWorker: (_workerId: string, label: string) =>
    label === "typescript" || label === "javascript"
      ? new TsWorker()
      : new EditorWorker(),
}

const exampleSources = import.meta.glob<string>("../../examples/*.ts", {
  eager: true,
  query: "?raw",
  import: "default",
})

const titleCase = (value: string) =>
  value
    .split("-")
    .map((word) => `${word[0]?.toUpperCase() ?? ""}${word.slice(1)}`)
    .join(" ")

const exampleIdFromPath = (path: string) =>
  path.split("/").at(-1)?.replace(/\.ts$/, "") ?? "example"

const commonPatterns = Object.entries(exampleSources)
  .map(([path, source]) => {
    const id = exampleIdFromPath(path)
    return { id, label: titleCase(id), source }
  })
  .sort((left, right) => left.label.localeCompare(right.label))

const patternIds = commonPatterns.map((pattern) => pattern.id)
const redirectStorageKey = "ts-pattern-swc-playground-redirect"

const takeRedirectPath = () => {
  try {
    const value = sessionStorage.getItem(redirectStorageKey)
    if (!value) return location.pathname
    sessionStorage.removeItem(redirectStorageKey)
    history.replaceState(null, "", value)
    return location.pathname
  } catch {
    return location.pathname
  }
}

const initialPath = takeRedirectPath()
const appBasePath = (() => {
  const marker = "/examples/"
  const index = initialPath.indexOf(marker)
  if (index >= 0) return initialPath.slice(0, index + 1) || "/"
  return initialPath.endsWith("/") ? initialPath : `${initialPath}/`
})()

const routeForPattern = (id: string) => `${appBasePath}examples/${id}`

const patternFromPath = (path = location.pathname) => {
  const match = path.match(/(?:^|\/)examples\/([^/]+)\/?$/)
  const id = match?.[1]
  return commonPatterns.find((pattern) => pattern.id === id) ??
    commonPatterns[0]
}

const DEFAULT_PATTERN = patternFromPath()
const DEFAULT_SOURCE = DEFAULT_PATTERN?.source ?? ""

const themeStorageKey = "ts-pattern-swc-playground-theme"
const tsPatternTypes = import.meta.glob<string>(
  "/node_modules/ts-pattern/dist/**/*.d.ts",
  { eager: true, query: "?raw", import: "default" },
)

type TransformOptions = {
  code: string
  plugin: boolean
  moduleType: ModuleType
}
type Theme = "white" | "dark"
type PatternId = typeof commonPatterns[number]["id"] | "custom"
type RunEntry = {
  input: string
  output: string
  rendered: unknown
}

type TypeScriptEditorProps = {
  value: string
  theme: Theme
  onChange: (value: string) => void
}

type CodeViewerProps = {
  value: string
  theme: Theme
  uri: string
}

const getInitialTheme = (): Theme => {
  try {
    return localStorage.getItem(themeStorageKey) === "dark" ? "dark" : "white"
  } catch {
    return "white"
  }
}

const applyTheme = (value: Theme) => {
  document.documentElement.dataset.theme = value
  monaco.editor.setTheme(value === "dark" ? "vs-dark" : "vs")
}

const configureTypeScript = () => {
  ts.typescriptDefaults.setCompilerOptions({
    allowNonTsExtensions: true,
    esModuleInterop: true,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.NodeJs,
    noEmit: true,
    strict: true,
    target: ts.ScriptTarget.ESNext,
  })
  ts.typescriptDefaults.setDiagnosticsOptions({
    noSemanticValidation: false,
    noSyntaxValidation: false,
  })
  for (const [path, content] of Object.entries(tsPatternTypes)) {
    ts.typescriptDefaults.addExtraLib(
      content,
      `file:///node_modules/ts-pattern/dist/${path.split("/dist/")[1]}`,
    )
  }
  ts.typescriptDefaults.addExtraLib(
    'export * from "./dist/index"',
    "file:///node_modules/ts-pattern/index.d.ts",
  )
}

configureTypeScript()

const source = signal(DEFAULT_SOURCE)
const compiledTsPattern = signal("")
const compiled = signal("")
const status = signal("Compiling")
const iterations = signal("100000")
const benchmarkStatus = signal("")
const runStatus = signal("")
const runEntries = signal<RunEntry[]>([])
const theme = signal(getInitialTheme())
const selectedPattern = signal<PatternId>(DEFAULT_PATTERN?.id ?? "custom")
let compileVersion = 0

applyTheme(theme.value)

const replaceCodeSpans = (
  code: string,
  replacements: Array<[string, string]>,
) => {
  let output = ""
  let index = 0
  let quote = ""
  let escaped = false
  while (index < code.length) {
    const char = code[index]
    if (quote) {
      output += char
      escaped = !escaped && char === "\\"
      if (!escaped && char === quote) quote = ""
      if (char !== "\\") escaped = false
      index += 1
      continue
    }
    if (char === '"' || char === "'" || char === "`") {
      quote = char
      output += char
      index += 1
      continue
    }
    const replacement = replacements.find(([from]) =>
      code.startsWith(from, index)
    )
    if (replacement) {
      output += replacement[1]
      index += replacement[0].length
      continue
    }
    output += char
    index += 1
  }
  return output
}

const formatCompiledJs = (code: string) =>
  replaceCodeSpans(code, [
    [";\nexport ", ";\n\nexport "],
    [" && ", "\n    && "],
    [" || ", "\n    || "],
    [" ? ", "\n  ? "],
    [" : ", "\n  : "],
  ]).replace(/\n{3,}/g, "\n\n")

const transformSource = async (options: TransformOptions) =>
  formatCompiledJs((await transformInBrowser(options)).code)

const compileSource = async (code: string) => {
  const version = ++compileVersion
  status.value = "Compiling"
  try {
    const [baselineOutput, pluginOutput] = await Promise.all([
      transformSource({
        code,
        plugin: false,
        moduleType: "es6",
      }),
      transformSource({
        code,
        plugin: true,
        moduleType: "es6",
      }),
    ])
    if (version !== compileVersion) return
    compiledTsPattern.value = baselineOutput
    compiled.value = pluginOutput
    status.value = ""
  } catch (error) {
    if (version !== compileVersion) return
    compiledTsPattern.value = ""
    compiled.value = ""
    status.value = error instanceof Error ? error.message : String(error)
  }
}

const isHtmlLike = (value: string) => /<\w[\s\S]*>/.test(value)

const resetOutputs = () => {
  benchmarkStatus.value = ""
  runStatus.value = ""
  runEntries.value = []
}

const pushPath = (path: string) => {
  if (location.pathname === path) return
  history.pushState(null, "", path)
}

const selectPattern = (
  pattern: typeof commonPatterns[number],
  options: { updatePath: boolean },
) => {
  selectedPattern.value = pattern.id
  source.value = pattern.source
  resetOutputs()
  if (options.updatePath) pushPath(routeForPattern(pattern.id))
  void compileSource(pattern.source)
}

addEventListener("popstate", () => {
  const pattern = patternFromPath()
  if (pattern) selectPattern(pattern, { updatePath: false })
})

const defaultBenchmarkInputs = [
  { type: "ok", value: 1 },
  { type: "ok", value: 2 },
  { type: "error", message: "failed" },
  { type: "idle" },
]

const runCurrent = async () => {
  runStatus.value = "Running"
  runEntries.value = []
  try {
    const code = await transformSource({
      code: source.value,
      plugin: true,
      moduleType: "commonjs",
    })
    const module = getModule(code)
    const inputs: unknown[] = module.inputs?.length
      ? module.inputs
      : defaultBenchmarkInputs
    const entries = inputs.map((input) => {
      const rendered = module.run(input)
      return {
        input: formatValue(input),
        output: formatValue(rendered),
        rendered,
      }
    })
    runEntries.value = entries
    runStatus.value = entries
      .map((entry) => `${entry.input} => ${entry.output}`)
      .join("\n")
  } catch (error) {
    runStatus.value = error instanceof Error ? error.message : String(error)
  }
}

const runBenchmark = async () => {
  benchmarkStatus.value = "Running"
  try {
    const count = Math.max(1, Math.floor(Number(iterations.value)))
    const [baselineCode, compiledCode] = await Promise.all([
      transformSource({
        code: source.value,
        plugin: false,
        moduleType: "commonjs",
      }),
      transformSource({
        code: source.value,
        plugin: true,
        moduleType: "commonjs",
      }),
    ])
    const baseline = getModule(baselineCode)
    const optimized = getModule(compiledCode)
    const baselineDeclaredInputs = baseline.inputs?.length
      ? baseline.inputs
      : defaultBenchmarkInputs
    const optimizedDeclaredInputs = optimized.inputs?.length
      ? optimized.inputs
      : defaultBenchmarkInputs
    const benchmarkInputs = prepareBenchmarkInputs({
      baseline,
      optimized,
      fallbackInputs: defaultBenchmarkInputs,
      generatedInputPairs: garbageInputPairsForInputs(
        baselineDeclaredInputs,
        optimizedDeclaredInputs,
        { seed: seedFromString(source.value), count: 64 },
      ),
    })

    const result = benchmarkModules({
      baseline: {
        code: baselineCode,
        run: baseline.run,
        inputs: benchmarkInputs.baselineInputs,
      },
      optimized: {
        code: compiledCode,
        run: optimized.run,
        inputs: benchmarkInputs.optimizedInputs,
      },
      count,
    })

    benchmarkStatus.value = formatBenchmarkComparison(result, {
      count,
      inputCount: benchmarkInputs.baselineInputs.length,
      generatedInputCount: benchmarkInputs.generatedInputCount,
      checkedInputCount: benchmarkInputs.checkedInputCount,
      rejectedInputCount: benchmarkInputs.rejectedInputCount,
      runtimeLabel: "browser benchmark",
    })
  } catch (error) {
    benchmarkStatus.value = error instanceof Error
      ? error.message
      : String(error)
  }
}

const onEditorChange = (value: string) => {
  source.value = value
  selectedPattern.value = "custom"
  resetOutputs()
  pushPath(appBasePath)
  void compileSource(value)
}

const onIterationsInput = (event: Event) => {
  iterations.value = (event.currentTarget as HTMLInputElement).value
}

const onThemeChange = (event: Event) => {
  const value = (event.currentTarget as HTMLSelectElement).value === "dark"
    ? "dark"
    : "white"
  theme.value = value
  applyTheme(value)
  try {
    localStorage.setItem(themeStorageKey, value)
  } catch {
    return
  }
}

const onPatternChange = (event: Event) => {
  const value = (event.currentTarget as HTMLSelectElement).value as PatternId
  const pattern = commonPatterns.find((item) => item.id === value)
  if (!pattern) return
  selectPattern(pattern, { updatePath: true })
}

const onPatternWheel = (event: WheelEvent) => {
  event.preventDefault()
  event.stopPropagation()
  const nextId = cycleValue(
    patternIds,
    selectedPattern.value === "custom"
      ? DEFAULT_PATTERN.id
      : selectedPattern.value,
    getWheelStep(event),
  )
  if (!nextId) return
  const pattern = commonPatterns.find((item) => item.id === nextId)
  if (!pattern) return
  selectPattern(pattern, { updatePath: true })
}

const TypeScriptEditor = (
  { value, theme, onChange }: TypeScriptEditorProps,
) => {
  const hostRef = useRef<HTMLDivElement>(null)
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null)
  const onChangeRef = useRef(onChange)
  const isUpdatingRef = useRef(false)

  onChangeRef.current = onChange

  useLayoutEffect(() => {
    if (!hostRef.current) return
    const model = monaco.editor.createModel(
      value,
      "typescript",
      monaco.Uri.parse("file:///playground.ts"),
    )
    const editor = monaco.editor.create(hostRef.current, {
      automaticLayout: true,
      fontFamily:
        "Sarasa Mono K Nerd Font, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
      fontSize: 14,
      minimap: { enabled: false },
      model,
      scrollBeyondLastLine: false,
      tabSize: 2,
      theme: theme === "dark" ? "vs-dark" : "vs",
    })
    editorRef.current = editor
    const subscription = editor.onDidChangeModelContent(() => {
      if (isUpdatingRef.current) return
      onChangeRef.current(editor.getValue())
    })
    return () => {
      subscription.dispose()
      editor.dispose()
      model.dispose()
      editorRef.current = null
    }
  }, [])

  // A delayed prop echo can overwrite newer keystrokes and reset the cursor.
  useLayoutEffect(() => {
    const editor = editorRef.current
    if (!editor || editor.getValue() === value) return
    isUpdatingRef.current = true
    editor.setValue(value)
    isUpdatingRef.current = false
  }, [value])

  useEffect(() => {
    monaco.editor.setTheme(theme === "dark" ? "vs-dark" : "vs")
  }, [theme])

  return <div class="monaco-host" ref={hostRef} />
}

const CodeViewer = ({ value, theme, uri }: CodeViewerProps) => {
  const hostRef = useRef<HTMLDivElement>(null)
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null)

  useLayoutEffect(() => {
    if (!hostRef.current) return
    const model = monaco.editor.createModel(
      value,
      "javascript",
      monaco.Uri.parse(uri),
    )
    const editor = monaco.editor.create(hostRef.current, {
      automaticLayout: true,
      fontFamily:
        "Sarasa Mono K Nerd Font, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
      fontSize: 14,
      minimap: { enabled: false },
      model,
      readOnly: true,
      scrollBeyondLastLine: false,
      tabSize: 2,
      theme: theme === "dark" ? "vs-dark" : "vs",
    })
    editorRef.current = editor
    return () => {
      editor.dispose()
      model.dispose()
      editorRef.current = null
    }
  }, [])

  useLayoutEffect(() => {
    const editor = editorRef.current
    if (!editor || editor.getValue() === value) return
    editor.setValue(value)
  }, [value])

  useEffect(() => {
    monaco.editor.setTheme(theme === "dark" ? "vs-dark" : "vs")
  }, [theme])

  return <div class="monaco-host" ref={hostRef} />
}

void compileSource(DEFAULT_SOURCE)

export const App = () => (
  <main>
    <section class="toolbar">
      <h1>Playground</h1>
      <div class="controls">
        <select
          value={selectedPattern.value}
          onChange={onPatternChange}
          onWheel={onPatternWheel}
        >
          {selectedPattern.value === "custom" && (
            <option value="custom" hidden>Custom</option>
          )}
          {commonPatterns.map((pattern) => (
            <option key={pattern.id} value={pattern.id}>{pattern.label}</option>
          ))}
        </select>
        <select value={theme.value} onChange={onThemeChange}>
          <option value="white">White</option>
          <option value="dark">Dark</option>
        </select>
        <label class="control-field">
          <span>Iterations/sample</span>
          <input
            value={iterations.value}
            onInput={onIterationsInput}
            inputMode="numeric"
          />
        </label>
        <button type="button" onClick={runCurrent}>Run</button>
        <button type="button" onClick={runBenchmark}>Benchmark</button>
      </div>
    </section>
    <section class="panes">
      <section class="pane-stack">
        <label class="pane">
          <span>TypeScript</span>
          <TypeScriptEditor
            value={source.value}
            theme={theme.value}
            onChange={onEditorChange}
          />
        </label>
        <label class="pane">
          <span>Compiled ts-pattern JS</span>
          <CodeViewer
            value={status.value || compiledTsPattern.value}
            theme={theme.value}
            uri="file:///compiled-ts-pattern.js"
          />
        </label>
      </section>
      <label class="pane">
        <span>Compiled SWC plugin JS</span>
        <CodeViewer
          value={status.value || compiled.value}
          theme={theme.value}
          uri="file:///compiled-plugin.js"
        />
      </label>
    </section>
    {runStatus.value && <pre class="run-output">{runStatus.value}</pre>}
    {runEntries.value.length > 0 && (
      <section class="rendered-results">
        <h2>Rendered result</h2>
        <div class="rendered-results-grid">
          {runEntries.value.map((entry, index) => (
            <article
              class="rendered-result-card"
              key={`${index}-${entry.input}`}
            >
              <pre>{entry.input}</pre>
              {typeof entry.rendered === "string"
                ? isHtmlLike(entry.rendered)
                  ? (
                    <div
                      class="rendered-result-preview"
                      dangerouslySetInnerHTML={{ __html: entry.rendered }}
                    />
                  )
                  : <div class="rendered-result-preview">{entry.rendered}</div>
                : <div class="rendered-result-preview">{entry.output}</div>}
            </article>
          ))}
        </div>
      </section>
    )}
    {benchmarkStatus.value && (
      <pre class="benchmark-output">{benchmarkStatus.value}</pre>
    )}
  </main>
)
