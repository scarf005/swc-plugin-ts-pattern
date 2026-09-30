# Release

1. Update `plugin/package.json`, `plugin/deno.json`, and `plugin/Cargo.toml` to the same version.
2. Run `just test`, the demo build/lint/browser checks, and `cd web && deno task build`.
   Runtime tests pack and install the npm archive in an isolated consumer project.
3. Run `cd plugin && deno publish --dry-run`, then commit the validated version change.
4. Create and push a matching tag:

```sh
git tag vX.Y.Z
git push origin main vX.Y.Z
```

Replace `X.Y.Z` with the manifest version.

The `Release` workflow publishes to both npm and JSR only from matching `v*` tags after the test job passes.

Required repository setup:

- npm: create `@scarf005/ts-pattern-swc-plugin`, then configure trusted publishing
  for `.github/workflows/release.yml`, or add an `NPM_TOKEN` repository secret.
- JSR: create/claim `@scarf005/ts-pattern-swc-plugin` and authorize GitHub Actions
  OIDC for this repository.
- GitHub: protect `main` by requiring the `Test` workflow check before merge.
