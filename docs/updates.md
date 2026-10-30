# App updates

`src/update.rs` keeps Syphr current from its GitHub releases.

## Checking

At launch and every 6 hours, while **Settings → Updates → Check for updates automatically** is on, Syphr reads the repository's releases from the GitHub API. It offers the highest version above its own that:

- isn't a draft,
- is a stable release, unless the running build is itself a beta (then betas count too),
- has this install's file (`Syphr-<tag>.dmg`, `-windows-x64.zip`, `-linux-x86_64.AppImage` when running as an AppImage, else `-linux-x64.tar.gz`) and a `SHA256SUMS`.

An available update shows as **Update to vX.Y.Z** at the bottom of the sidebar and on the Updates settings page, which also has **Check for updates**.

## Installing

1. Downloads the file and `SHA256SUMS`, and checks the file's hash in memory before writing anything.
2. Replaces the app in place, keeping the old copy until the new one is in:
   - **macOS** mounts the DMG with `hdiutil`, copies `Syphr.app` beside the running bundle with `ditto`, then swaps the two.
   - **Windows and Linux** unpack the archive with the OS's `tar` and swap the executable. Windows can rename but not delete a running exe, so the old one (`syphr-dl.exe.old`) is removed on the next launch. The NSIS installer installs per user, so this works on an installed copy too.
   - **AppImage** (detected by the `APPIMAGE` variable its runtime sets) swaps the new AppImage over the file in `$APPIMAGE`.
3. Starts the new copy and quits.

Development builds (`cargo run`) and a binary outside `Syphr.app` refuse to update. The app's folder must be writable by the user, so an install in a system directory has to be updated by hand. A `.deb` install (an executable under `/usr`) refuses and asks for the new `.deb`.

Running downloads stop when Syphr restarts, as they do on Quit.
