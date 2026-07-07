# LinuxSCP Client

Ein eigenständiger, nativer **Dual-Pane-Dateiübertragungs-Client** für Linux —
links das lokale Dateisystem, rechts ein entfernter Server. Der Workflow
orientiert sich am vertrauten Zwei-Fenster-Konzept klassischer FTP-Clients
(schnelle Navigation, Drag-and-Drop, Transfer-Warteschlange), ist aber ein
komplett eigenes UI ohne fremde Marken, Logos oder Assets.

> **Status:** Phase 1–3. Implementiert: **SFTP, SCP, FTP, FTPS**, gespeicherte
> Sitzungen, Zwei-Pane-Browser, Upload/Download inkl. rekursiver Verzeichnisse,
> globale Transfer-Warteschlange mit Pause/Fortsetzen/Abbrechen/Wiederholen,
> sichere Passwortablage im System-Keyring (pure Rust), Host-Key-Prüfung,
> Konfliktdialog (Überschreiben/Fortsetzen/Umbenennen/Überspringen, für alle),
> Transfer-Resume ab Offset, **SHA-256-Verifikation** (lokal ↔ remote),
> **Verzeichnis-Synchronisierung**, **eingebauter Texteditor** (Doppelklick,
> lokal & remote, speichert direkt zurück), **Tabs für mehrere gleichzeitige
> Sitzungen**, größenverstellbare Panels, verschiebbare Dialoge und abdockbare
> Fenster. WebDAV/S3 und Terminal sind über die Protokoll-Abstraktion vorbereitet.

### Protokoll-Fähigkeiten

| Fähigkeit | SFTP | SCP | FTP / FTPS |
|---|:--:|:--:|:--:|
| Browsen, mkdir, rm, rename | ✅ | ✅ (via SFTP) | ✅ |
| Rechte anzeigen/ändern (chmod) | ✅ | ✅ | ⚠ nur `SITE CHMOD` |
| Besitzer/Zeitstempel | ✅ | ✅ | teilweise |
| Transfer-Resume | ✅ | ❌ | ✅ (REST) |
| SHA-256-Verifikation | ✅ (`sha256sum`) | ✅ | ❌ |

SCP nutzt dieselbe SSH-Verbindung: Browsen/Metadaten laufen über SFTP, die
eigentliche Byte-Übertragung über das SCP-Protokoll.

---

## Technologiewahl

| Schicht | Technologie | Warum |
|---|---|---|
| Shell/Packaging | **Tauri 2** | winzige Bundles, natives `.deb`/`.rpm`/AppImage, hartes Trust-Boundary UI↔Core |
| Core | **Rust** | Speichersicherheit, echtes Multithreading, abbrechbare Hintergrund-Tasks |
| SSH/SFTP | **ssh2** (libssh2, gevendored) | ausgereifteste SFTP/SCP-Implementierung; keine Distro-Abhängigkeit |
| Secrets | **keyring** → Secret Service | Passwörter/Passphrasen nie im Klartext |
| UI | **React + TypeScript + Vite** | freies, modernes, HiDPI-fähiges Design; Dark/Light |
| State | **zustand** | schlanker Store für Verbindung, Transfers, Theme |

Das WebView hat **keinen** direkten Datei- oder Netzwerkzugriff — jede Operation
läuft über eine typisierte Tauri-Command-Fassade im Rust-Core.

## Architektur

```
Frontend (React/TS)  ──invoke()──▶  Tauri Commands  ──▶  Core (Rust)
   DualPane / ConnectionManager        (commands.rs)        protocol::FileProvider
   TransferQueue / StatusBar      ◀──events──               ├─ LocalProvider
                                  (queue-update,             └─ SftpProvider
                                   transfer-progress)        transfer::Queue (tokio + CancellationToken)
                                                             session::Store (JSON, ohne Secrets)
                                                             secrets (Keyring)  ·  logging (tracing)
```

Kernstück ist der Trait [`FileProvider`](src-tauri/src/protocol/mod.rs): jedes
Protokoll implementiert dieselbe Schnittstelle, sodass FTP/FTPS/WebDAV/S3 später
ergänzt werden können, ohne UI oder Warteschlange zu ändern. Blockierende I/O
läuft ausschließlich in `spawn_blocking`; Transfers sind pausier-/abbrechbare
Tasks, die den UI-Thread nie blockieren.

### Projektstruktur

```
linuxscp-client/
├─ src/                       # Frontend
│  ├─ App.tsx                 # Orchestrierung, Event-Wiring, Transfer-Trigger
│  ├─ api.ts                  # typisierte invoke()-Wrapper + Event-Listener
│  ├─ state/store.ts          # zustand-Store
│  ├─ util.ts                 # Pfad-, Größen-, Sortier-Helfer
│  └─ components/             # FilePane, ConnectionManager, TransferQueue,
│                             #   HostKeyDialog, StatusBar
├─ src-tauri/                 # Rust-Core
│  ├─ src/
│  │  ├─ protocol/            # FileProvider-Trait, local.rs, sftp.rs
│  │  ├─ session/             # Store (JSON-Persistenz der Sitzungen)
│  │  ├─ transfer/            # Queue (Warteschlange, Fortschritt, Cancel)
│  │  ├─ secrets.rs           # Keyring
│  │  ├─ commands.rs          # Tauri-Command-Fassade
│  │  ├─ error.rs · logging.rs · lib.rs · main.rs
│  ├─ tests/local_transfer.rs # Integrationstests der Transfer-Primitive
│  ├─ capabilities/           # Tauri-Berechtigungen
│  └─ tauri.conf.json
└─ scripts/gen-icons.mjs      # Platzhalter-Icons ohne Abhängigkeiten
```

---

## Voraussetzungen

- **Node.js ≥ 20** und **Rust ≥ 1.77** (via [rustup](https://rustup.rs))
- Tauri-Systempakete:

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

> OpenSSL und libssh2 werden aus Quelltext gebaut (vendored) — kein `libssl-dev`
> nötig. Die Keyring-Anbindung ist reines Rust (zbus) — **kein** `libsecret`/
> `libdbus` zur Bauzeit. Zur **Laufzeit** muss ein Secret-Service-Provider laufen
> (GNOME Keyring bzw. KWallet — bei GNOME/KDE-Desktops Standard).

## Setup & Entwicklung

```bash
npm install
npm run gen:icons        # Platzhalter-Icons erzeugen (einmalig)
npm run app:dev          # Vite + Tauri im Dev-Modus (Hot-Reload)
```

Reines Frontend ohne Desktop-Fenster: `npm run dev` (nur zur UI-Ansicht;
`invoke`-Aufrufe funktionieren erst im Tauri-Fenster).

## Build & Packaging

```bash
npm run app:build
```

Erzeugt in `src-tauri/target/release/bundle/`:

- `deb/…deb` (Debian/Ubuntu)
- `rpm/…rpm` (Fedora)
- `appimage/…AppImage` (distributionsübergreifend)

Für ein echtes Logo statt der Platzhalter-Icons:

```bash
npm run tauri icon assets/logo.png    # generiert den vollständigen Icon-Satz
```

## Tests

```bash
npm run test:rust     # Rust-Unit- + Integrationstests
npm run lint          # ESLint (TS/React)
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

Abgedeckt:
- **Pfadlogik** (`protocol::posix_join/normalize/parent/basename`)
- **Verbindungsdaten** (`session::Store` — Serialisierung enthält nie Secrets)
- **Transfer-Primitive** (`tests/local_transfer.rs` — Fortschritt, Cancellation,
  **Resume ab Offset**)

CI (GitHub Actions, `.github/workflows/ci.yml`): Lint, Format, Clippy (als
Fehler), Tests und optionaler Bundle-Build mit Artefakt-Upload.

---

## Sicherheitskonzept

- **Keine Klartext-Secrets.** Passwörter und Key-Passphrasen liegen im
  System-Keyring (Secret Service), referenziert über `<session-id>:<kind>`.
  Die Sitzungs-JSON (`~/.config/linuxscp-client/sessions.json`) enthält
  ausschließlich nicht-sensible Verbindungsdaten — abgesichert durch einen
  Serialisierungs-Test.
- **Host-Key-Prüfung.** Beim Verbinden wird der Host-Key gegen
  `~/.ssh/known_hosts` geprüft. Unbekannte oder **geänderte** Keys lösen einen
  ausdrücklichen Bestätigungsdialog aus (mit SHA-256-Fingerprint und deutlicher
  Warnung bei Änderung); erst nach Zustimmung wird der Key gespeichert. Keine
  stille Akzeptanz.
- **Sichere Defaults.** libssh2 nutzt die bevorzugten modernen KEX/Cipher; die
  WebView läuft unter restriktiver CSP; das Frontend hat keinen FS-/Netzzugriff.
- **Keine Secrets in Logs.** Logging (`tracing`) erhält an den Aufrufstellen
  keine Geheimnisse; `logging::redact` maskiert defensiv.
- **Robuste Fehler & Abbrüche.** Netzwerk-/Transferfehler werden typisiert an
  die UI gemeldet (technischer Detailtext im Fehler), Transfers sind sauber
  abbrechbar; Verbindungs- und Operationstimeouts sind gesetzt.

**Grenzen des MVP** (siehe Roadmap): Wiederholungslogik ist einfach (manuelles
„Wiederholen"); Transfer-Resume und Prüfsummen-Vergleich sind vorbereitet, aber
noch nicht aktiv; pro Verbindung wird eine SSH-Session seriell genutzt.

## Bedienung (Kurzreferenz)

- **＋ Verbindung** öffnet den Sitzungsmanager (Suche, Anlegen, Verbinden). Jede
  Verbindung wird als **eigener Tab** geöffnet — mehrere Server gleichzeitig; Tab
  wechseln merkt sich den Remote-Ordner, `✕` bzw. Mittelklick schließt ihn. Das
  lokale Panel ist über alle Tabs gemeinsam.
- **Doppelklick** auf Ordner navigiert; **Doppelklick auf eine Textdatei** öffnet
  sie im **eingebauten Editor** (lokal & remote, Strg+S speichert direkt zurück).
  Binärdateien wie `.jar`, Archive, Bilder usw. werden bewusst nicht geöffnet.
  **Breadcrumb-Doppelklick** editiert den Pfad; **Backspace** = übergeordneter
  Ordner.
- **Drag-and-Drop** zwischen den Panels startet Upload/Download; alternativ die
  Pfeil-Buttons in der Mitte oder das Kontextmenü.
- **Entf** löschen, **F2** umbenennen, **F5** aktualisieren.
- Kontextmenü: Öffnen / „Im Editor öffnen", Übertragen, Umbenennen, Rechte
  ändern (chmod), Löschen.
- **⇄** im Mittelbalken öffnet die **Verzeichnis-Synchronisierung** (Richtung
  wählen → Analysieren → Übertragen; es wird nichts gelöscht).

**Fenster & Layout**
- **Panels größenverstellbar:** die Trennlinie zwischen Lokal/Remote ziehen
  (Spaltenbreite), und die obere Kante der Warteschlange ziehen (Höhe).
- **Dialoge frei verschiebbar:** Verbindungen, Konflikt- und Host-Key-Dialog am
  Titel greifen und im Fenster bewegen.
- **Abdockbare Einzelfenster:** das ⧉-Symbol öffnet ein Panel oder die
  Warteschlange als eigenes, vom Betriebssystem verschiebbares Fenster. Es
  arbeitet gegen denselben Backend-Zustand (Verbindungen/Queue leben im
  Rust-Core) — die abgedockte Warteschlange spiegelt alle Transfers, ein
  abgedocktes Panel navigiert live weiter. (Transfers werden aus abgedockten
  Panels bewusst nicht ausgelöst — dafür das Hauptfenster nutzen.)

## Roadmap (weitere Phasen)

1. `russh`-Migration für gepipelinete (bei Latenz deutlich schnellere)
   SFTP-Transfers.
2. Eingebautes SSH-Terminal.
3. WebDAV- und S3-kompatibler Provider.
4. Cross-Window-Drag-and-Drop zwischen abgedockten Fenstern.

Erledigt: SFTP/SCP/FTP/FTPS · Konfliktdialog · Resume · SHA-256 ·
Verzeichnis-Sync · eingebauter Editor · Tabs · Fenster-/Layout-Funktionen.

## Lizenz & Marken

Eigenständiges Projekt. Es werden keine Namen, Logos, Icons, Screenshots oder
sonstigen Assets fremder Produkte verwendet. Der Arbeitstitel lautet
„LinuxSCP Client".
