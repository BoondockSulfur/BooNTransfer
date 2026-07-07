# Packaging — BooN Transfer

Linux integration files, shared by the `.deb`/`.rpm`/AppImage bundles and the
Flatpak.

| File | Purpose |
|------|---------|
| `os.boon.transfer.desktop` | Desktop entry (menu launcher, MIME/keywords). `Exec=boon-transfer` matches the binary name set via `mainBinaryName`. |
| `os.boon.transfer.metainfo.xml` | AppStream metadata for GNOME Software / KDE Discover / Flathub. |
| `flatpak/os.boon.transfer.yml` | Flatpak manifest (GNOME runtime → webkit2gtk). |

App ID / reverse-DNS: **`os.boon.transfer`** (same as the Tauri `identifier`).
License: **MIT**. Binary: **`boon-transfer`**.

## deb / rpm / AppImage

`npm run tauri build` generates these. Tauri writes its own `.desktop` from the
config, but ship `os.boon.transfer.metainfo.xml` into
`/usr/share/metainfo/` so software centres show the app properly. The
`os.boon.transfer.desktop` here is the canonical source if you want to override
Tauri's generated one.

## Flatpak

The manifest unpacks the built `.deb`. From the repo root:

```sh
npm run tauri build
cp "src-tauri/target/release/bundle/deb/BooN Transfer_0.1.0_amd64.deb" \
   packaging/flatpak/boon-transfer.deb
flatpak-builder --user --install --force-clean build-dir \
   packaging/flatpak/os.boon.transfer.yml
flatpak run os.boon.transfer
```

**finish-args rationale** (least privilege):

- `--share=network` — it is a network file-transfer client.
- `--talk-name=org.freedesktop.secrets` — system keyring for stored passwords.
- `--filesystem=home` — browse/transfer local files.
- `--filesystem=~/.ssh` — read `known_hosts` and key files for host-key
  verification and key auth.
- Wayland + fallback X11 + `--device=dri` for the webview.

**Not yet verified** with `flatpak-builder` in this environment. Confirm the
runtime version (webkit2gtk availability) and the keyring portal on a real
build before publishing to Flathub or the BooN-OS repo.
