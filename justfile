web:
    cd web && npm ci && npm run dev

# Lints, formats and tests everything except the browser tests.
test:
    npm ci
    npm run lint
    npm run fmt -- --check
    cd plugin && npm ci && npm test
    cd web && npm ci && npm run check && npm test
    just check-examples
    just test-vendored-ts-pattern

# Runs the Playwright playground tests in headless Chromium.
test-browser:
    cd web && npm ci && env -u WAYLAND_DISPLAY -u DISPLAY npm run test:browser

# Runs the fuzz test of generated match chains `runs` times.
fuzz runs="20000":
    cd plugin && npm run build && FUZZ_RUNS={{ runs }} node --test --test-name-pattern="^generated chains" tests/differential.test.mjs

bench:
    cd web && npm ci --silent && npm run --silent bench

check-examples:
    cd examples && npm ci && npm run check

test-vendored-ts-pattern:
    cd vendor/ts-pattern && npm ci
    node scripts/check-vendored-ts-pattern-swc.mts
    cd vendor/ts-pattern && npm test

# Rewrites scripts/vendored-fallbacks.json with the vendored matches that stay on the runtime.
update-vendored-fallbacks:
    cd vendor/ts-pattern && npm ci
    node scripts/check-vendored-ts-pattern-swc.mts --update-fallbacks

bench-ts-pattern: bench

bench-file:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p docs
    NO_COLOR=1 just bench | sed "s|{{justfile_directory()}}/||" > docs/just-bench.txt

update-readme-bench: bench-file
    npx --yes automd --input plugin/README.md

# Fills the README with the Benchmark workflow result for HEAD.
update-readme-bench-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    run=$(gh run list --workflow bench.yml --commit "$(git rev-parse HEAD)" --status success --limit 1 --json databaseId --jq ".[0].databaseId")
    dir=$(mktemp -d)
    gh run download "$run" --name benchmark --dir "$dir"
    mv "$dir/just-bench.txt" docs/just-bench.txt
    npx --yes automd --input plugin/README.md

# Tests, builds and publishes the plugin to npm; a prerelease gets its label as dist-tag.
release:
    cd plugin && npm publish --tag "$(node -p "require('./package.json').version.match(/-([a-z]+)/)?.[1] ?? 'latest'")"
