# Release

1. Set the same version in `plugin/package.json` and `plugin/Cargo.toml`.
2. Run `just test`, then commit the version change.
3. Run `just release`. npm runs `npm test` (`prepublishOnly`) and builds the WASM (`prepack`) before upload. A prerelease version such as `X.Y.Z-beta.N` gets the matching dist-tag (`beta`). Other versions get `latest`.
4. Tag the release and push it:

```sh
git tag vX.Y.Z
git push origin main vX.Y.Z
```
