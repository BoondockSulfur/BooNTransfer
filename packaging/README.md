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

`npm run tauri build` generates these (verified: all three build; binary
`/usr/bin/boon-transfer`, brand icon in hicolor). The installed `.desktop` comes
from [`linux/boon-transfer.desktop.hbs`](linux/boon-transfer.desktop.hbs) (wired
via `bundle.linux.{deb,rpm}.desktopTemplate`) so it lands under **Network /
FileTransfer** with search keywords.

Tauri names that file `BooN Transfer.desktop` (after the product name). AppStream
metainfo integration for the traditional packages is therefore left to the
Flatpak (below), where the desktop id is the reverse-DNS `os.boon.transfer`. To
ship the metainfo in the deb/rpm too, install `os.boon.transfer.metainfo.xml`
into `/usr/share/metainfo/` and set its `<launchable>` to `BooN Transfer.desktop`
to match.

## Flatpak

The manifest unpacks the built `.deb`. From the repo root:

```sh
npm run tauri build
cp "src-tauri/target/release/bundle/deb/BooN Transfer_0.1.0_amd64.deb" \
   packaging/flatpak/boon-transfer.deb
# Keep build-dir and state OUT of the repo — the build tree unpacks a root fs
# with symlink loops that otherwise crash the Vite dev watcher (ELOOP).
flatpak-builder --user --install --force-clean \
   --state-dir "$HOME/.cache/boon-transfer-flatpak" \
   /tmp/boon-transfer-flatpak-build \
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

**Verified** with `flatpak-builder` on **GNOME Platform 49** (a supported
runtime): builds and installs, `libwebkit2gtk-4.1` resolves inside the sandbox,
and the permissions apply (`network`, wayland/x11, `~/.ssh`, `home`, and
`org.freedesktop.secrets` for the keyring). The desktop/icon/metainfo export
under the `os.boon.transfer` app id. First run the setup once:

```sh
bash packaging/flatpak/setup-build-env.sh
```
