# v2 auf russh — Plan (BooN-OS-Flaggschiff)

> Status: **Aktiver v2-Workspace.** Dies ist die Kopie, in der der russh-Umbau
> stattfindet. Das Original (`~/dev/linuxscp-client`) bleibt unangetastet und
> weiter nutzbar.
>
> **Arbeitstitel & Kennungen (provisorisch, jederzeit umbenennbar):**
> - Ordner: `~/dev/boon-transfer`
> - Produktname / Fenstertitel: `BooN Transfer`
> - Bundle-Identifier: `os.boon.transfer`
> - Config-Ordner / Keyring-Dienst / Logs: `boon-transfer` (getrennt vom Original,
>   damit beide Apps unabhängig laufen — gespeicherte Sitzungen/Passwörter kollidieren nicht)
> - Rust-Crate/Lib heißen intern noch `linuxscp-client`/`linuxscp_lib` (wird beim
>   Umbau umbenannt; rein intern, keine Kollision).
>
> **Phase 1 erledigt (Prototyp + Benchmark).** Zwei Erkenntnisse:
> 1. Serielles russh-sftp ist langsam (kein Read-Ahead). **Selbst gebautes
>    Pipelining** (viele parallele Requests via `RawSftpSession`) dreht das um:
>    **schneller als ssh2 bei 0 ms, gleichauf bis ~25 ms RTT, Upload überall
>    mindestens gleichauf.** Bei 50 ms noch etwas unter libssh2s Read-Ahead —
>    Feintuning-Aufgabe, kein Blocker (ssh2 blockierend kann Pipelining gar nicht).
> 2. Für den realistischen Fall (naher Server) ist **russh = so schnell/schneller**.
>    Details/Zahlen: `experiments/russh-spike/README.md`.
>
> **Folge:** Bedingung „genauso schnell" ist für den realen Einsatz **erfüllt**;
> zusammen mit der pure-Rust-Basis ist die russh-Migration für BooN-OS **tragfähig
> und empfohlen**.
>
> **Phase 2 KERN FERTIG & verifiziert:** async `FileProvider`, LocalProvider
> (tokio::fs), Transfer-Queue (async, Pause=Cancel+Resume) und der neue
> **russh-SftpProvider mit Pipelining** kompilieren (build/clippy/fmt/8 Tests grün)
> und laufen **end-to-end gegen echten sshd** (connect/host-key/auth/list/
> upload/download/sha256/stat/remove — `cargo run --example smoke`).
> **Offen (nächste Schritte):** SSH-Agent-Auth (aktuell Stub — wichtig für
> Hetzner-Key), FTP/FTPS auf async-suppaftp+rustls zurückholen (aktuell gestubbt),
> Frontend-Altlasten (`editRemote`/Editor-Events) entfernen, GUI-Lauftest.

## 0. TL;DR / Empfehlung

- **Kein Rewrite von Null.** Stattdessen **Re-Architektur**: React-Frontend +
  Tauri-Shell + Session-/Keyring-/Fehler-/Queue-Konzept behalten, nur den
  **SSH-Transport async auf `russh` neu** bauen. Frontend-Reuse ~85–90 %.
- **Kernargument ist nicht nur Tempo, sondern eine pure-Rust-Basis:** `russh` +
  `russh-sftp` + `rustls` ersetzen **libssh2 + gevendorte OpenSSL + native-tls**.
  → kleiner/schneller/reproduzierbar bauen, keine OpenSSL-CVE-Pflege, einfachere
  Lizenz, Memory-Safety, gepipelintes (latenzresistentes) SFTP.
- **Ziel:** ein sauber paketierbares, mehrsprachiges, barrierearmes Standard-Tool,
  das in BooN-OS mitgeliefert werden kann.

## 1. Warum russh für eine Distro (nicht nur Speed)

| | heute (v1) | v2 (russh) |
|---|---|---|
| SSH | libssh2 (C) | russh (Rust) |
| Krypto | gevendorte OpenSSL (C-Build) | ring/aws-lc-rs (Rust + geprüftes asm) |
| FTPS-TLS | native-tls → OpenSSL | rustls (Rust) |
| SFTP-Durchsatz bei Latenz | round-trip-gebunden | gepipelint |
| Lizenz-Fußabdruck | + OpenSSL-Lizenz | Apache-2.0/MIT, kein OpenSSL |
| Build | langsam (OpenSSL/libssh2 aus Quelle) | schneller, reproduzierbarer |

> Nuance zur „pure Rust": Der Krypto-Backend von russh nutzt `ring` (Rust +
> Assembler) bzw. `aws-lc-rs` (etwas geprüftes C). **Entscheidend:** kein
> System-`libssh2`/`openssl` mehr, kein OpenSSL-Quellbuild. Backend-Wahl als
> Feature-Flag festlegen (Ziel: `ring` für maximale Rust-Reinheit).

## 2. Was bleibt, was wird neu

**Bleibt (übernehmen, ggf. minimal anpassen):**
- Gesamtes **React/TS-Frontend**: DualPane, Tabs, ConnectionManager, Konflikt-/
  Sync-/Host-Key-/Editor-Dialoge, TransferQueue-UI, Detached-Fenster, Theming.
- **Tauri-Shell**, Capabilities, Packaging-Grundgerüst.
- **Session-Store** (JSON, ohne Secrets), **Keyring** (async-secret-service),
  **error.rs**, **logging**.
- **Command-Fassade** (Signaturen bleiben weitgehend; Rückgabetypen stabil).

**Wird neu (async, russh):**
- `protocol`-Schicht: **`FileProvider` wird async** (oder Actor-Bridge, s. u.).
- **SftpProvider** auf `russh` + `russh-sftp`.
- **Transfer-Queue** async (tokio-Tasks statt spawn_blocking-Loops; Pause/Cancel
  via `tokio::select!` + `CancellationToken`).
- **FTP/FTPS** auf `suppaftp` mit **rustls** statt native-tls (C-frei).

## 3. Ziel-Architektur (async)

```
Frontend (React/TS)  ──invoke()──▶  Tauri Commands (async)
                     ◀──events──                │
                                                ▼
                       protocol::FileProvider (async trait)
                        ├─ LocalProvider      (tokio::fs)
                        ├─ SftpProvider        (russh + russh-sftp)
                        └─ FtpProvider         (suppaftp + rustls)
                       transfer::Queue (tokio tasks, select! cancel/pause)
                       session::Store · secrets · hostkey · logging
```

### 3a. Async-Provider: zwei Optionen

1. **Async-Trait (empfohlen für v2):** `FileProvider` mit `async fn`
   (via `#[async_trait]` oder native async-fn-in-trait). Queue `await`et die
   Methoden. Sauberste Lösung, passt zu russh.
2. **Connection-Actor (Bridge, falls weniger Umbau gewünscht):** je Verbindung
   eine dedizierte tokio-Task, die den russh-Client besitzt; Commands schicken
   Requests über einen Channel. Hält die restliche Schicht synchron. Nützlich,
   um schrittweise zu migrieren.

> Entscheidung: **v2 geht Async-Trait.** Die Queue nutzt bereits
> `CancellationToken`; Pause wird ein `Notify`/`watch`-Signal, das im
> Transfer-Loop per `select!` abgefragt wird.

### 3b. russh-Design (die kniffligen Teile)

- **Handshake/Connect:** `russh::client::connect` mit `Config`; eigener
  `Handler`.
- **Host-Key-Prüfung:** im `Handler::check_server_key(&self, key)` — dort gegen
  `~/.ssh/known_hosts` prüfen (Crate `russh-keys`/eigene known_hosts-Logik) und
  bei unbekannt/geändert denselben `HostKeyVerification`-Fehler an die UI geben
  wie heute. **Verhalten 1:1 wie v1 beibehalten.**
- **Auth:** Passwort (`authenticate_password`), Key (`russh-keys` lädt auch
  verschlüsselte Keys mit Passphrase → `authenticate_publickey`), **Agent** über
  `russh_keys::agent::client::AgentClient`.
- **SFTP:** `russh-sftp`-Client auf einem `channel_open_session` + `request_subsystem("sftp")`.
  Deckt readdir/stat/lstat/mkdir/remove/rename/setstat/realpath/symlink/open ab.
  **Pipelining** durch mehrere offene Read-Requests → hier kommt der Speed her.
- **Resume:** SFTP open + seek/offset (wie heute).
- **SCP:** optional über Exec-Kanal beibehalten; oder in v2 SCP auf „Transfer via
  SFTP" reduzieren (SCP-Modus-Flag kann bleiben, nutzt intern schnelles SFTP).
- **Prüfsumme:** `sha256sum` weiterhin über Exec-Kanal (`channel.exec`).

## 4. Distro- & Public-Release-Themen (neu ggü. v1)

- **i18n:** UI aktuell Deutsch hartkodiert → **Sprachdateien** (de/en mind.),
  einfache i18n-Lib oder eigenes Dictionary. Systemsprache erkennen.
- **Barrierefreiheit:** Tastatur-Vollbedienung (teils vorhanden), ARIA-Rollen,
  Fokus-Reihenfolge, Kontraste (WCAG AA) in beiden Themes prüfen.
- **Packaging:** `.deb`/`.rpm`/AppImage (haben wir) **+ Flatpak** (für breite
  Distro-Reichweite; Flatpak-Manifest, Secret-Service-Portal beachten).
- **Signierung & Updates:** AppImage/Flatpak signieren; optional Tauri-Updater
  mit signierten Releases. Für BooN-OS ggf. ins Distro-Repo statt Selbst-Update.
- **Branding:** eigener Name + Icon für BooN-OS (Kandidaten: „BooN Transfer",
  „BooNSCP", „Boon Files"). Lizenz festlegen (Vorschlag **MIT oder Apache-2.0**).
- **Doku:** README + kurze Nutzer-Hilfe (in-App „?"), Manpage optional.
- **Telemetrie:** keine (bewusst) — passt zu Distro-Ethos.

## 4b. FTP/FTPS — ERLEDIGT

Für ein Distro-Flaggschiff gehört FTP/FTPS dazu (breiter Nutzerkreis, Webhosting).
Umgesetzt auf **async `suppaftp` + rustls** (pure Rust). Plain FTP **end-to-end
gegen echten Server verifiziert** (connect/auth/list/upload/download/remove ✓,
`cargo run --example ftp_smoke`). FTPS nutzt rustls mit Mozilla-Roots
(`webpki-roots`) und **validiert Zertifikate by default** — Live-Test gegen einen
Server mit gültigem Zertifikat noch offen (lokaler Self-Signed würde korrekt
abgelehnt). Bridging: generischer futures→tokio Read-Helfer (Download) +
`tokio_util::compat` (Upload); Plain/TLS hinter einem `Conn`-Enum.

<details><summary>Warum das anfangs fragil schien (historisch)</summary>

- `suppaftp` async läuft auf **async-std** (zweite Runtime neben tokio) statt tokio.
- Transfers brauchen **`tokio_util::compat`**-Bridging (async-std/futures ↔ tokio).
- **FTPS-TLS** erfordert versionsgenaues Zusammenspiel von `suppaftp` ↔
  `futures-rustls` ↔ `rustls` (Connector-Typ muss exakt passen).
- Plain- vs. TLS-Stream sind **verschiedene Typen** → Enum + Stream-Rückgabe pro
  Variante kollidiert (retr_as_stream liefert je Variante anderen Typ).

Der Dual-Stream-Typ war der eigentliche Knackpunkt — gelöst durch einen
**generischen `stream_to_dst`-Helfer** je Enum-Arm (statt eines gemeinsamen
Stream-Bindings). Der Rest fügte sich sauber (Compile lief im ersten Anlauf).
</details>

## 5. Tests & CI

- **Integrationstests gegen echten sshd** im Container (z. B. `linuxserver/openssh`
  oder `testcontainers`) → connect/list/upload/download/resume/checksum real testen.
- **FTP/FTPS** gegen `vsftpd`/`pure-ftpd`-Container.
- **Latenz-Benchmark**-Job (künstliche RTT via `tc netem`) → russh vs. v1 belegen.
- Pure-Logik-Unit-Tests (Pfade, Queue, Sync-Diff, Binär-Erkennung) übernehmen.
- CI-Matrix: Ubuntu/Fedora/Arch-Build + Flatpak-Build.

## 6. Phasenplan (wenn wir starten)

1. **Spike/Benchmark (risikofrei):** russh+russh-sftp-Prototyp: connect + readdir +
   1 Datei up/down; Benchmark gegen v1 mit `tc netem`-Latenz. → Go/No-Go-Beleg.
2. **Async-Kern:** `FileProvider` async; LocalProvider (tokio::fs); Queue async
   (Pause/Cancel via select!). Frontend unverändert.
3. **SftpProvider (russh):** Host-Key + Auth (Passwort/Key/Agent) + alle FS-Ops +
   Resume + Exec-Checksum. v1-Feature-Parität herstellen, Integrationstests grün.
4. **FTP/FTPS auf rustls** umstellen (C-frei).
5. **Distro-Politur:** i18n, a11y, Flatpak, Branding, Signierung, Doku.
6. **Release-Kandidat:** Paket-Tests auf BooN-OS, Beta.

## 7. Risiken & Gegenmaßnahmen

| Risiko | Gegenmaßnahme |
|---|---|
| Feature-Regression beim SSH-Neubau | Erst **Prototyp+Benchmark**, dann Parität gegen v1-Feature-Checkliste + Integrationstests, ehe v1 abgelöst wird |
| russh-API-Feinheiten (Host-Key/Agent) | Gegen Compiler + echten sshd iterieren (wie bei ssh2 bewährt) |
| Async-Umbau der Queue (Pause/Cancel) | select!-Muster früh isoliert testen |
| Flatpak + Keyring (Secret-Service-Portal) | Portal-Zugriff im Manifest testen; Fallback dokumentieren |
| „pure Rust" überverkauft (aws-lc-rs) | Krypto-Backend `ring` wählen, ehrlich dokumentieren |

## 8. Offene Entscheidungen (für dich)

1. **Re-Architektur (Reuse)** vs. echtes Fork/Neuprojekt mit neuem Namen?
   (Empfehlung: Re-Architektur, neuer Name/Branding als Layer obendrauf.)
2. **Name & Lizenz** für die BooN-OS-Variante.
3. **Protokoll-Umfang** v2: SFTP+FTP/FTPS wie heute — SCP behalten oder als
   SFTP-Alias führen? WebDAV/S3 gleich mitplanen?
4. **Krypto-Backend:** `ring` (max. Rust-Reinheit) vs. `aws-lc-rs` (Default)?
5. **Update-Weg:** Distro-Repo vs. Tauri-Selbst-Updater?
6. **i18n-Sprachen** zum Start (de/en – mehr?).
