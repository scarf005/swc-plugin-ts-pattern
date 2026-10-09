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

The `Release` workflow publishes to JSR and stages the npm package only from matching `v*` tags after the test job passes. Approve the staged npm version with `npm stage approve <stage-id>` or on npmjs.com. Prerelease versions such as `X.Y.Z-alpha.N`, `X.Y.Z-beta.N`, and `X.Y.Z-rc.N` use the matching npm dist-tag; other versions use `latest`.

Required repository setup:

- npm: publish the first version manually, then configure a stage-only trusted publisher
  for `.github/workflows/release.yml` and disallow tokens.
- JSR: create/claim `@scarf005/swc-plugin-ts-pattern` and authorize GitHub Actions
  OIDC for this repository.
- GitHub: protect `main` by requiring the `Test` workflow check before merge.
