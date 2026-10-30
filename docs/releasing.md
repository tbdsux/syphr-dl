# CI and releases

Two GitHub Actions workflows, on macOS, Windows and Ubuntu.

## Test — `.github/workflows/test.yml`

Runs on every push to `main` and every pull request, on macOS, Windows and Ubuntu (Linux first frees disk space with [free-disk-space](https://github.com/endersonmenezes/free-disk-space), as the release Linux job does, then installs the X11, Wayland and font headers GPUI links against):

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Network tests (`#[ignore]`) don't run in CI.

## Release — `.github/workflows/release.yml`

Runs when a `v*` tag is pushed. It follows the [GPUI Kit packaging guide](https://gpui-kit.com/docs/packaging/):

1. Fails if the tag doesn't match `version` in `Cargo.toml`. The macOS, Windows and Linux builds then run in parallel.
2. Builds `aarch64-apple-darwin` and `x86_64-apple-darwin` and joins them into one universal binary with `lipo`.
3. Assembles `Syphr.app` with the icon from `assets/icon/Syphr.icns` and an `Info.plist` (identifier `dev.tbdsux.syphr-dl`, version from the tag, build number from the run number).
4. Signs it ad hoc and verifies the signature.
5. Packs `Syphr-vX.Y.Z.dmg` with an Applications shortcut.
6. Alongside, a Windows job builds `syphr-dl.exe`, zips it as `Syphr-vX.Y.Z-windows-x64.zip`, and packs a per-user NSIS installer, `Syphr-vX.Y.Z-windows-x64-setup.exe`.
7. A Linux job builds `syphr-dl` on Ubuntu 22.04 (for an older glibc baseline) and packs it as `Syphr-vX.Y.Z-linux-x64.tar.gz`, `Syphr-vX.Y.Z-linux-amd64.deb` and `Syphr-vX.Y.Z-linux-x86_64.AppImage`.
8. Once all builds pass, publishes a GitHub release with every file, a `SHA256SUMS` of them, and generated notes. The in-app updater only offers releases that have `SHA256SUMS` (see [updates.md](updates.md)).

The installers come from [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) (pinned in `release.yml`), configured under `[package.metadata.packager]` in `Cargo.toml`. It names its output `syphr-dl_<version>_<arch>…`, so the jobs rename each file to the `Syphr-<tag>…` names the updater looks for.

## Cutting a release

1. Bump `version` in `Cargo.toml`, then run `cargo build` so `Cargo.lock` updates.
2. Commit and push to `main`.
3. Tag and push:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

A tag with a suffix, like `v0.2.0-beta.1`, is published as a prerelease. The suffix is dropped from the macOS bundle version, which must be numeric.

## Signing and notarization

Releases are signed ad hoc, so macOS warns on first launch. To ship signed, notarized builds you need an Apple Developer account, then in `release.yml`:

1. Import a *Developer ID Application* certificate into a temporary keychain from repository secrets.
2. Sign with `codesign --force --timestamp --options runtime --sign "Developer ID Application: …"`.
3. Submit the DMG with `xcrun notarytool submit … --wait`, check it's *Accepted*, then `xcrun stapler staple`.

The step to replace is marked with a `shortcut:` comment.

Windows builds are unsigned, so SmartScreen warns on first launch. Signing needs a code-signing certificate and a `signtool sign` step after the build, and the installer signed the same way (also marked with a `shortcut:` comment).
