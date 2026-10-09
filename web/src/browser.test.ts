/// <reference lib="deno.ns" />
import { assert, assertEquals } from "jsr:@std/assert"
import { chromium, type Page } from "playwright"
import type { editor } from "monaco-editor/editor/editor.api.js"

const webRoot = new URL("../", import.meta.url)

const withPlayground = async (
  run: (page: Page, origin: string) => Promise<void>,
) => {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 })
  const port = (listener.addr as Deno.NetAddr).port
  listener.close()
  const origin = `http://127.0.0.1:${port}`
  const server = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "-A",
      "--node-modules-dir",
      "vite",
      "--host",
      "127.0.0.1",
      "--port",
      String(port),
      "--strictPort",
    ],
    cwd: webRoot,
    stdout: "null",
    stderr: "inherit",
  }).spawn()
  let serverExited = false
  const serverStatus = server.status.then((status) => {
    serverExited = true
    return status
  })
  let browser
  try {
    const deadline = Date.now() + 30_000
    while (true) {
      if (serverExited) throw new Error("Playground dev server exited")
      try {
        const response = await fetch(origin)
        await response.body?.cancel()
        if (response.ok) break
      } catch { /* Wait for the actual server, not an arbitrary sleep. */ }
      if (Date.now() >= deadline) {
        throw new Error("Playground dev server did not start")
      }
      await new Promise((resolve) => setTimeout(resolve, 100))
    }
    browser = await chromium.launch()
    const page = await browser.newPage()
    // Cold WASM compilation also shares CPU with other CI jobs.
    page.setDefaultTimeout(120_000)
    await run(page, origin)
  } finally {
    try {
      await browser?.close()
    } finally {
      if (!serverExited) server.kill("SIGTERM")
      await serverStatus
    }
  }
}

const editorModuleUrl = async (page: Page) =>
  await page.evaluate(async () => {
    const source = await (await fetch("/src/app.tsx")).text()
    const match = source.match(/import \* as monaco from "([^"]+)"/)
    if (!match) throw new Error("Served Monaco import missing")
    return match[1]
  })

const waitForCompilation = async (page: Page, url: string) => {
  await page.waitForFunction(async (url) => {
    const monaco = await import(url)
    const models: editor.ITextModel[] = monaco.editor.getModels()
    const output = models.find((model) =>
      model.uri.toString() === "file:///compiled-plugin.js"
    )
    return models.length === 3 && output?.getValue().includes("export") &&
      !output.getValue().includes(".with(")
  }, url)
}

Deno.test("playground renders, compiles, restores history, edits and runs benchmarks", () =>
  withPlayground(async (page, origin) => {
    const errors: string[] = []
    page.on("pageerror", (error) => errors.push(error.message))
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text())
    })
    await page.goto(origin)
    const moduleUrl = await editorModuleUrl(page)
    await waitForCompilation(page, moduleUrl)
    assertEquals(await page.locator(".monaco-host").count(), 3)
    await page.getByRole("combobox").first().selectOption("literals")
    await waitForCompilation(page, moduleUrl)
    await page.getByRole("combobox").first().selectOption("objects")
    await waitForCompilation(page, moduleUrl)
    await page.goBack()
    assertEquals(
      await page.getByRole("combobox").first().inputValue(),
      "literals",
    )
    await page.goForward()
    assertEquals(
      await page.getByRole("combobox").first().inputValue(),
      "objects",
    )
    await page.getByRole("textbox", { name: "Iterations/sample" }).fill("1000")
    await page.getByRole("button", { name: "Benchmark", exact: true }).click()
    await page.locator(".benchmark-output").filter({
      hasText: "browser benchmark",
    }).waitFor().catch(async (error) => {
      throw new Error(
        `${error.message}\n${await page.locator("body").innerText()}`,
      )
    })
    const benchmarkText = await page.locator(".benchmark-output").textContent()
    assert(benchmarkText?.includes("checked"))
    assert(!/Infinity|NaN/.test(benchmarkText ?? ""))
    await page.goto(`${origin}/subdir/examples/literals`)
    await waitForCompilation(page, moduleUrl)
    await page.getByRole("textbox", { name: "Editor content" }).first().focus()
    await page.keyboard.press("ControlOrMeta+A")
    const customSource =
      'import { match } from "ts-pattern"; export const inputs = [1, 2]; export const run = (value: number) => match(value).with(1, () => "one").otherwise(() => "other");'
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"])
    await page.evaluate(
      (text) => navigator.clipboard.writeText(text),
      customSource,
    )
    await page.keyboard.press("ControlOrMeta+V")
    await page.keyboard.press("End")
    await page.keyboard.press("Enter")
    await page.keyboard.type("// typed")
    const editedSource = `${customSource}\n// typed`
    await page.waitForFunction(() =>
      location.pathname === "/subdir/" &&
      document.querySelector("select")?.value === "custom"
    )
    assertEquals(
      await page.evaluate(
        async (url) =>
          (await import(url)).editor.getModels().find((
            model: editor.ITextModel,
          ) => model.uri.toString() === "file:///playground.ts").getValue(),
        moduleUrl,
      ),
      editedSource,
    )
    await page.waitForFunction(
      async (url) =>
        (await import(url)).editor.getModels().find((
          model: editor.ITextModel,
        ) => model.uri.toString() === "file:///compiled-plugin.js")?.getValue()
          .includes('"one"'),
      moduleUrl,
    )
    await page.getByRole("button", { name: "Run", exact: true }).click()
    await page.waitForFunction(() =>
      document.querySelectorAll(".rendered-result-card").length === 2
    ).catch(async (error) => {
      throw new Error(
        `${error.message}\n${await page.locator("body").innerText()}`,
      )
    })
    assertEquals(
      await page.locator(".rendered-result-card").allTextContents(),
      ["1one", "2other"],
    )
    assertEquals(errors, [])
  }))

Deno.test("playground retries failed SWC and plugin downloads without reloading", () =>
  withPlayground(async (page, origin) => {
    await page.goto(origin)
    const moduleUrl = await editorModuleUrl(page)
    for (const asset of ["**/swc_plugin_ts_pattern.wasm", "**/wasm_bg.wasm"]) {
      await page.route(
        asset,
        (route) =>
          route.fulfill({ status: 503, body: "download failure fixture" }),
      )
      await page.goto(`${origin}/examples/literals`)
      await page.waitForFunction(
        async (url) =>
          (await import(url)).editor.getModels().some((
            model: editor.ITextModel,
          ) =>
            /Failed to load|503|WebAssembly|CompileError/.test(model.getValue())
          ),
        moduleUrl,
      )
      await page.unroute(asset)
      await page.getByRole("combobox").first().selectOption("objects")
      await waitForCompilation(page, moduleUrl)
    }
  }))

Deno.test("playground keeps editors usable after rendering multi-row results", () =>
  withPlayground(async (page, origin) => {
    await page.goto(`${origin}/examples/literals`)
    await waitForCompilation(page, await editorModuleUrl(page))
    await page.getByRole("button", { name: "Run", exact: true }).click()
    await page.waitForFunction(() =>
      document.querySelectorAll(".rendered-result-card").length === 7
    )
    const heights = await page.locator(".monaco-host").evaluateAll((hosts) =>
      hosts.map((host) => host.getBoundingClientRect().height)
    )
    assert(
      heights.every((height) => height >= 120),
      `Collapsed editors: ${heights.join(", ")}`,
    )
  }))
