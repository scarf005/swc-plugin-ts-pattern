# Playground

```sh
deno task dev
deno task bench
```

## Browser regression tests

```sh
deno run -A playwright install chromium
deno task test:browser
```

The tests start isolated dev servers and cover initial compilation, example history, keyboard editing under a URL prefix, Run, benchmark/property checks, and recovery after failed SWC or plugin downloads. CI installs Chromium's system dependencies with `--with-deps`.

The `bench` task compares the vendored `ts-pattern-benchmark` cases across raw `ts-pattern`, native code, and plugin output, then compares every example with `Deno.bench`.

The playground runs the plugin-enabled SWC wasm binding in the browser and passes the Rust plugin bytes through the wasm binding's experimental plugin bytes resolver. The `prepare-assets` task copies the plugin wasm into `public/`, and the production build copies it into `dist/`. Cloudflare Pages serves routes through `dist/_redirects`.
