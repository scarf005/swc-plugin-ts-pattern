web:
    cd web && deno task dev

test:
    cd plugin && npm ci
    cargo test --manifest-path plugin/Cargo.toml
    cd plugin && npm run build
    cd plugin && npm run test:runtime
    cd web && deno task check
    cd web && deno task test
    just check-examples
    just test-vendored-ts-pattern

bench:
    cd web && deno task bench

check-examples:
    cd examples && deno task check

test-vendored-ts-pattern:
    cd vendor/ts-pattern && npm ci
    deno run -A --unstable-detect-cjs scripts/check-vendored-ts-pattern-swc.ts
    cd vendor/ts-pattern && npm test

bench-ts-pattern: bench

bench-file:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p docs
    NO_COLOR=1 just bench | sed "s|file://{{justfile_directory()}}/||" > docs/just-bench.txt

update-readme-bench: bench-file
    deno run -A npm:automd --input plugin/README.md

# Fills the README with the Benchmark workflow result for HEAD.
update-readme-bench-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    run=$(gh run list --workflow bench.yml --commit "$(git rev-parse HEAD)" --status success --limit 1 --json databaseId --jq ".[0].databaseId")
    dir=$(mktemp -d)
    gh run download "$run" --name benchmark --dir "$dir"
    mv "$dir/just-bench.txt" docs/just-bench.txt
    deno run -A npm:automd --input plugin/README.md
