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

update-readme-bench:
    mkdir -p docs
    NO_COLOR=1 just bench > docs/just-bench.txt
    deno run -A npm:automd --input plugin/README.md
