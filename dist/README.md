# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/zeron-<version>-linux-<arch>.tar.gz` containing:

- `zeron` — the binary (headed by default; `zeron headless` runs the engine alone)
- `zeron.desktop` — XDG desktop entry
- `zeron.png` — 1024×1024 Mast app icon
- `install.sh` — installs into `~/.local/{bin,share/applications,share/icons}`

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/zeron-<version>-macos-<arch>.dmg
```

Builds the release binary, assembles `Mast.app` (Info.plist + icns), ad-hoc
signs it (set `CODESIGN_IDENTITY` for a real Developer ID), and wraps it in a
dmg. The auto-update tarball retains an internal `Zeron.app` path so older
installed builds can update into Mast. Release asset file names keep the
`zeron-` prefix (the update manifest, release workflow and download links key
on them); the dmg volume and the app inside it are named Mast. CI runs this on tags
(`.github/workflows/release.yml`). The manual steps it automates, for reference
(run on a macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p mast --target aarch64-apple-darwin
   cargo build --release -p mast --target x86_64-apple-darwin
   lipo -create -output zeron \
     target/aarch64-apple-darwin/release/zeron \
     target/x86_64-apple-darwin/release/zeron
   ```
2. Assemble the bundle:
   ```sh
   mkdir -p Mast.app/Contents/{MacOS,Resources}
   cp zeron Mast.app/Contents/MacOS/zeron
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     dist/macos/Info.plist > Mast.app/Contents/Info.plist
   ```
3. Icon: generate `mast.icns` from `dist/macos/icon-1024.png` (the macOS-shaped
   variant of the artwork — squircle mask, margins, and shadow pre-baked, since
   `sips` can't apply an alpha mask) and place it at
   `Mast.app/Contents/Resources/mast.icns`:
   ```sh
   mkdir mast.iconset && sips -z 256 256 dist/macos/icon-1024.png --out mast.iconset/icon_256x256.png
   iconutil -c icns mast.iconset -o Mast.app/Contents/Resources/mast.icns
   ```
4. Sign + notarize (required for distribution):
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" Mast.app
   xcrun notarytool submit Mast.zip --keychain-profile … --wait
   xcrun stapler staple Mast.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Mast -srcfolder Mast.app -ov -format UDZO Mast.dmg`).
