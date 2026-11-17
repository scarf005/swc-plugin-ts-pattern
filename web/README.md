# Playground

```sh
npm ci
npm run dev
npm run bench
```

## Browser regression tests

```sh
npx playwright install chromium
npm run test:browser
```

The tests start isolated dev servers and cover initial compilation, example history, keyboard editing under a URL prefix, Run, benchmark/property checks, and recovery after failed SWC or plugin downloads. CI installs Chromium's system dependencies with `--with-deps`.

The `bench` script compares the vendored `ts-pattern-benchmark` cases across raw `ts-pattern`, native code, and plugin output, then compares every example with Vitest benchmarks.

The playground runs the plugin-enabled SWC wasm binding in the browser and passes the Rust plugin bytes through the wasm binding's experimental plugin bytes resolver. The `prepare-assets` script copies the plugin wasm into `public/`, and the production build copies it into `dist/`. The [`Pages` workflow](../.github/workflows/pages.yml) builds with `BASE_PATH=/swc-plugin-ts-pattern/` and deploys to GitHub Pages, which serves `dist/404.html` (a copy of `index.html`) for `/examples/<id>` routes. Cloudflare Pages reads `dist/_redirects` instead.
