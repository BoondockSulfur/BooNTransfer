# BooN Transfer

Ein nativer **Dual-Pane-Dateiübertragungs-Client** für Linux — links das lokale
Dateisystem, rechts ein entfernter Server. Vertrauter Zwei-Fenster-Workflow
(schnelle Navigation, Drag-and-Drop, Transfer-Warteschlange) in einem komplett
eigenen UI ohne fremde Marken oder Assets. Entwickelt als Bordmittel für
**[BooN-OS](https://github.com/BoondockSulfur)**.

**Kein libssh2, kein OpenSSL** (anders als v1, das libssh2 + vendored OpenSSL
nutzte). SSH läuft über **russh** (reines Rust, RustCrypto); FTPS-TLS über
**rustls** (Krypto-Provider `aws-lc-rs` — AWS-LC, nicht OpenSSL).

> **Protokolle:** **SFTP, SCP, FTP, FTPS, WebDAV, WebDAVS, S3** (und
> S3-kompatibel: MinIO, Backblaze …). Gespeicherte Sitzungen · Zwei-Pane-Browser
> · globale Transfer-Warteschlange mit Pause/Fortsetzen/Abbrechen/Wiederholen ·
> Passwörter im System-Keyring (pure Rust) · Host-Key-Prüfung ·
> Passwort-/Key-/**Agent**-Auth · Konfliktdialog · Transfer-Resume ·
> **SHA-256-Verifikation** · **Verzeichnis-Synchronisierung** · **eingebautes
> SSH-Terminal** · **Editor in eigenen Fenstern** · **Tabs** · abdockbare Fenster
> · **signierte Self-Updates** · **Deutsch/Englisch** · Hell/Dunkel.

### Protokoll-Fähigkeiten

| Fähigkeit | SFTP | SCP | FTP / FTPS | WebDAV | S3 |
|---|:--:|:--:|:--:|:--:|:--:|
| Browsen, mkdir, rm, rename | ✅ | ✅ (via SFTP) | ✅ | ✅ | ✅¹ |
| Rechte anzeigen/ändern (chmod) | ✅ | ✅ | ⚠ nur `SITE CHMOD` | ❌ | ❌ |
| Transfer-Resume | ✅ | ❌ | ✅ (REST) | ❌ | ❌ |
| Gepipelinete Transfers (schnell bei Latenz) | ✅ | ✅ | — | — | — |
| SHA-256-Verifikation | ✅ (`sha256sum`) | ✅ | ❌ | ❌ | ❌ |

¹ S3 kennt keine echten Ordner (Präfixe + `name/`-Marker); Umbenennen = Copy+Delete.

SFTP-Transfers sind **gepipelinet** (viele gleichzeitige Read/Write-Requests) —
das gleicht Latenz aus und hält mit libssh2-basierten Clients mit bzw. übertrifft
sie. SCP nutzt dieselbe SSH-Verbindung: Browsen/Metadaten über SFTP, die
Byte-Übertragung über das SCP-Protokoll.

---

## Technologiewahl

| Schicht | Technologie | Warum |
|---|---|---|
| Shell/Packaging | **Tauri 2** | winzige Bundles, natives `.deb`/`.rpm`/AppImage/Flatpak, hartes Trust-Boundary UI↔Core |
| Core | **Rust (async, tokio)** | Speichersicherheit, echtes Multithreading, abbrechbare Transfers |
| SSH/SFTP | **russh + russh-sftp** | reines Rust (RustCrypto), kein libssh2/OpenSSL; selbst gebautes Request-Pipelining |
| FTP/FTPS | **suppaftp + rustls** | TLS via rustls (`aws-lc-rs`, kein OpenSSL) mit Zertifikatsprüfung (Mozilla-Roots) |
| WebDAV | **reqwest (rustls) + quick-xml** | PROPFIND/MKCOL/DELETE/MOVE/GET/PUT über HTTP(S) |
| S3 | **rusty-s3 + reqwest** | SigV4-Presign, S3-kompatibel (AWS/MinIO/Backblaze) |
| Terminal | **russh-PTY + xterm.js** | interaktive SSH-Shell in eigenem Fenster |
| Secrets | **keyring** → Secret Service (zbus) | Passwörter/Passphrasen nie im Klartext |
| UI | **React + TypeScript + Vite** | modernes, HiDPI-fähiges Design; Dark/Light; i18n |
| State | **zustand** | schlanker Store für Verbindung, Transfers, Theme, Sprache |

Das WebView hat **keinen** direkten Datei- oder Netzwerkzugriff — jede Operation
läuft über eine typisierte Tauri-Command-Fassade im Rust-Core.

## Architektur

```
Frontend (React/TS)  ──invoke()──▶  Tauri Commands  ──▶  Core (Rust, async)
   DualPane / ConnectionManager        (commands.rs)        protocol::FileProvider (async trait)
   TransferQueue / StatusBar      ◀──events──               ├─ LocalProvider (tokio::fs)
   i18n (de/en) · Themes          (queue-update,            ├─ SftpProvider (russh, pipelined)
                                   transfer-progress)        └─ FtpProvider  (suppaftp + rustls)
                                                             transfer::Queue (tokio + CancellationToken)
                                                             session::Store (JSON, ohne Secrets)
                                                             secrets (Keyring)  ·  logging (tracing)
```

Kernstück ist der async-Trait
[`FileProvider`](src-tauri/src/protocol/mod.rs): jedes Protokoll implementiert
dieselbe Schnittstelle, sodass weitere Provider (WebDAV/S3) ergänzt werden
können, ohne UI oder Warteschlange zu ändern. Transfers sind pausier-/abbrechbare
async-Tasks, die den UI-Thread nie blockieren.

### Projektstruktur

```
boon-transfer/
├─ src/                       # Frontend
│  ├─ App.tsx                 # Orchestrierung, Event-Wiring, Transfer-Trigger
│  ├─ api.ts                  # typisierte invoke()-Wrapper + Event-Listener
│  ├─ i18n.ts                 # de/en-Wörterbuch + useT()-Hook
│  ├─ state/store.ts          # zustand-Store (Theme, Sprache, Transfers …)
│  ├─ hooks.ts                # useDraggable, useDialog (a11y)
│  └─ components/             # FilePane, ConnectionManager, TransferQueue,
│                             #   HostKeyDialog, ConflictDialog, SyncDialog, EditorModal, StatusBar
├─ src-tauri/                 # Rust-Core
│  ├─ src/
│  │  ├─ protocol/            # FileProvider-Trait, local.rs, sftp.rs (russh), ftp.rs (suppaftp)
│  │  ├─ session/             # Store (JSON-Persistenz der Sitzungen, ohne Secrets)
│  │  ├─ transfer/            # Queue (Warteschlange, Fortschritt, Cancel, Resume)
│  │  ├─ secrets.rs · commands.rs · error.rs · logging.rs · lib.rs · main.rs
│  ├─ examples/               # smoke.rs (SFTP), ftp_smoke.rs (FTP) — End-to-End gegen echte Server
│  ├─ tests/local_transfer.rs # Integrationstests der Transfer-Primitive
│  └─ tauri.conf.json
├─ packaging/                 # .desktop, AppStream-Metainfo, Flatpak-Manifest, Icon-Quelle (SVG)
└─ docs/V2-RUSSH-PLAN.md      # Architektur-/Umbau-Plan
```

---

## Voraussetzungen

- **Node.js ≥ 20** und **Rust ≥ 1.77** (via [rustup](https://rustup.rs))
- Tauri-Systempakete (nur für die GUI/WebView — **nicht** für Krypto):

**Debian/Ubuntu**
```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  build-essential pkg-config file
```
**Fedora**
```bash
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel librsvg2-devel \
  pkgconf-pkg-config @development-tools
```
**Arch**
```bash
sudo pacman -S --needed webkit2gtk-4.1 gtk3 librsvg base-devel pkgconf
```

> Der SSH-/TLS-Stack ist reines Rust (russh/rustls) — **kein** `libssl-dev`,
> **kein** libssh2. Die Keyring-Anbindung ist reines Rust (zbus) — **kein**
> `libsecret`/`libdbus` zur Bauzeit. Zur **Laufzeit** muss ein Secret-Service-
> Provider laufen (GNOME Keyring bzw. KWallet — bei GNOME/KDE Standard).

## Setup & Entwicklung

```bash
npm install
npm run app:dev          # Vite + Tauri im Dev-Modus (Hot-Reload), Fenster öffnet auf Port 1420
```

Reines Frontend ohne Desktop-Fenster: `npm run dev` (nur zur UI-Ansicht;
`invoke`-Aufrufe funktionieren erst im Tauri-Fenster).

## Build & Packaging

```bash
npm run app:build
```

Erzeugt in `src-tauri/target/release/bundle/`: `deb/…deb`, `rpm/…rpm`,
`appimage/…AppImage`. Das installierte Binary heißt **`boon-transfer`**.

**Flatpak** und die Linux-Integrationsdateien (`.desktop`, AppStream-Metainfo)
liegen in [`packaging/`](packaging/) — Build-Schritte siehe
[`packaging/README.md`](packaging/README.md).

Icon neu generieren (aus der Marken-SVG):
```bash
npm run tauri icon packaging/icon/boon-transfer.svg
```

## Tests

```bash
npm run test:rust     # Rust-Unit- + Integrationstests
npm run lint          # ESLint (TS/React)
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

End-to-End gegen echte Server (Docker):
```bash
cargo run --example smoke     -- <host> <port> <user> <pass|agent> <dir>   # SFTP
cargo run --example ftp_smoke -- <host> <port> <user> <pass> <dir>         # FTP
```

CI (GitHub Actions, `.github/workflows/ci.yml`): Lint, Format, Clippy (als
Fehler), Tests (`--locked`) und optionaler Bundle-Build mit Artefakt-Upload.

---

## Sicherheitskonzept

- **Kein OpenSSL/libssh2.** SSH über **russh** (reines Rust, RustCrypto), FTPS
  über **rustls** mit dem gut auditierten `aws-lc-rs`-Provider (AWS-LC, nicht
  OpenSSL). Moderne Default-Algorithmen; FTPS validiert Server-Zertifikate gegen
  die Mozilla-Roots.
- **Keine Klartext-Secrets.** Passwörter und Key-Passphrasen liegen im
  System-Keyring (Secret Service), referenziert über `<session-id>:<kind>`. Die
  Sitzungs-JSON (`~/.config/boon-transfer/sessions.json`) enthält ausschließlich
  nicht-sensible Verbindungsdaten — abgesichert durch einen Serialisierungs-Test.
- **Host-Key-Prüfung.** Beim Verbinden wird der Host-Key gegen
  `~/.ssh/known_hosts` geprüft. Unbekannte oder **geänderte** Keys lösen einen
  ausdrücklichen Bestätigungsdialog aus (SHA-256-Fingerprint, deutliche Warnung
  bei Änderung); erst nach Zustimmung wird der Key gespeichert. Keine stille
  Akzeptanz.
- **Auth-Optionen.** Passwort, SSH-Schlüsseldatei (mit Passphrase) und
  **SSH-Agent** (Schlüssel aus dem laufenden Agent).
- **Sichere Defaults.** Die WebView läuft unter restriktiver CSP; das Frontend
  hat keinen FS-/Netzzugriff.
- **Keine Secrets in Logs.** Logging (`tracing`) erhält an den Aufrufstellen
  keine Geheimnisse; `logging::redact` maskiert defensiv.
- **Robuste Fehler & Abbrüche.** Fehler werden typisiert an die UI gemeldet;
  Transfers sind sauber abbrechbar; Timeouts sind gesetzt.

## Barrierefreiheit & Sprachen

- **Deutsch/Englisch**, Systemsprache wird erkannt; Umschalter oben rechts.
- Dialoge sind als `role="dialog"`/`alertdialog` ausgezeichnet, **Escape**
  schließt, der Fokus wandert in den Dialog. Sichtbare Tastatur-Fokusringe in
  beiden Themes; sortierbare Tabellenköpfe sind echte Buttons mit `aria-sort`.

## Bedienung (Kurzreferenz)

- **＋ Verbindung** öffnet den Sitzungsmanager (Suche, Anlegen, Verbinden). Jede
  Verbindung öffnet als **eigener Tab**; `✕` bzw. Mittelklick schließt ihn. Das
  lokale Panel ist über alle Tabs gemeinsam.
- **Doppelklick** auf Ordner navigiert; **Doppelklick auf eine Textdatei** öffnet
  sie im **eingebauten Editor** (lokal & remote, Strg+S speichert direkt zurück).
  Binärdateien (`.jar`, Archive, Bilder …) werden bewusst nicht geöffnet.
- **Drag-and-Drop** zwischen den Panels startet Upload/Download; alternativ die
  Pfeil-Buttons in der Mitte oder das Kontextmenü.
- **Entf** löschen, **F2** umbenennen, **F5** aktualisieren.
- **⇄** im Mittelbalken öffnet die **Verzeichnis-Synchronisierung** (Richtung
  wählen → Analysieren → Übertragen; es wird nichts gelöscht).
- **⧉** dockt ein Panel oder die Warteschlange als eigenes Fenster ab (arbeitet
  gegen denselben Backend-Zustand).

## Updates

Installierte **AppImages aktualisieren sich selbst** — unabhängig von der Distro.
Beim Start prüft die App ein GitHub-Release; ist eine neuere Version signiert
verfügbar, bietet ein Banner *Installieren & neu starten*. Der Download wird
gegen den eingebetteten Public Key verifiziert, bevor er angewandt wird.
`.deb`/`.rpm` aktualisieren über den Paketmanager. Details:
[RELEASING.md](RELEASING.md).

## Roadmap

- S3-Multipart-Uploads (aktuell werden Uploads gepuffert).
- WebDAV-/S3-Uploads streamen (kein RAM-Puffer).

Erledigt: russh-Kern mit Pipelining · **SFTP/SCP/FTP/FTPS/WebDAV/S3** ·
Agent-Auth · **eingebautes SSH-Terminal** · Konfliktdialog · Resume · SHA-256 ·
Verzeichnis-Sync · **Editor in eigenen Fenstern** · fensterübergreifende
Übertragung · Tabs · abdockbare Fenster · i18n (de/en) · Barrierefreiheit ·
MIT-Lizenz · Packaging (deb/rpm/appimage/Flatpak) · **signierte Self-Updates**.

## Lizenz

**MIT** — siehe [LICENSE](LICENSE). Eigenständiges Projekt; es werden keine
Namen, Logos, Icons oder sonstigen Assets fremder Produkte verwendet.
