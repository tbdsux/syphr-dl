# CI and releases

Two GitHub Actions workflows, both on `macos-latest`.

## Test — `.github/workflows/test.yml`

Runs on every push to `main` and every pull request:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Network tests (`#[ignore]`) don't run in CI.

## Release — `.github/workflows/release.yml`

Runs when a `v*` tag is pushed. It follows the [GPUI Kit packaging guide](https://gpui-kit.com/docs/packaging/):

1. Fails if the tag doesn't match `version` in `Cargo.toml`.
2. Builds `aarch64-apple-darwin` and `x86_64-apple-darwin` and joins them into one universal binary with `lipo`.
3. Assembles `Syphr.app` with an `Info.plist` (identifier `dev.tbdsux.syphr-dl`, version from the tag, build number from the run number).
4. Signs it ad hoc and verifies the signature.
5. Packs `Syphr-vX.Y.Z.dmg` with an Applications shortcut.
6. Publishes a GitHub release with the DMG and generated notes.

## Cutting a release

1. Bump `version` in `Cargo.toml`, then run `cargo build` so `Cargo.lock` updates.
2. Commit and push to `main`.
3. Tag and push:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

## Signing and notarization

Releases are signed ad hoc, so macOS warns on first launch. To ship signed, notarized builds you need an Apple Developer account, then in `release.yml`:

1. Import a *Developer ID Application* certificate into a temporary keychain from repository secrets.
2. Sign with `codesign --force --timestamp --options runtime --sign "Developer ID Application: …"`.
3. Submit the DMG with `xcrun notarytool submit … --wait`, check it's *Accepted*, then `xcrun stapler staple`.

The step to replace is marked with a `shortcut:` comment.
