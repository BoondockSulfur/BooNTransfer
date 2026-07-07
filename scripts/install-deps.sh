#!/usr/bin/env bash
#
# LinuxSCP Client — Build-Abhängigkeiten installieren (idempotent).
# Prüft alle nötigen System- und Rust-Tools und installiert nur, was fehlt.
# sudo wird ausschließlich aufgerufen, wenn tatsächlich Pakete fehlen.
#
set -euo pipefail

log()  { printf '\033[1;36m▶ %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m! %s\033[0m\n' "$*"; }

# --- 1) System-Pakete (Tauri: GTK/WebKit + Build-Tools) --------------------
detect_pm() {
  if command -v apt-get >/dev/null 2>&1; then echo apt
  elif command -v dnf     >/dev/null 2>&1; then echo dnf
  elif command -v pacman  >/dev/null 2>&1; then echo pacman
  else echo unknown
  fi
}

PM="$(detect_pm)"
log "Paketmanager: $PM"

declare -a PKGS
case "$PM" in
  apt)
    PKGS=(libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev build-essential pkg-config file curl)
    is_installed() { dpkg -s "$1" >/dev/null 2>&1; }
    install_cmd()  { sudo apt-get update && sudo apt-get install -y "$@"; }
    ;;
  dnf)
    PKGS=(webkit2gtk4.1-devel gtk3-devel librsvg2-devel pkgconf-pkg-config gcc gcc-c++ make file curl)
    is_installed() { rpm -q "$1" >/dev/null 2>&1; }
    install_cmd()  { sudo dnf install -y "$@"; }
    ;;
  pacman)
    PKGS=(webkit2gtk-4.1 gtk3 librsvg base-devel pkgconf file curl)
    is_installed() { pacman -Qi "$1" >/dev/null 2>&1; }
    install_cmd()  { sudo pacman -S --needed --noconfirm "$@"; }
    ;;
  *)
    warn "Unbekannter Paketmanager — bitte GTK3-, WebKit2GTK-4.1-, librsvg- und Build-Tools manuell installieren."
    PKGS=()
    ;;
esac

missing=()
for p in "${PKGS[@]:-}"; do
  if [ -n "$p" ] && is_installed "$p"; then ok "$p"; else missing+=("$p"); fi
done

if [ "${#missing[@]}" -gt 0 ]; then
  warn "Fehlend: ${missing[*]}"
  log "Installiere fehlende Pakete (sudo-Passwort wird evtl. abgefragt) …"
  install_cmd "${missing[@]}"
  ok "System-Pakete installiert."
else
  ok "Alle System-Pakete bereits vorhanden — nichts zu tun."
fi

# --- 2) Rust-Toolchain -----------------------------------------------------
if command -v cargo >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo" ]; then
  ok "Rust/cargo vorhanden ($("${HOME}/.cargo/bin/cargo" --version 2>/dev/null || cargo --version))"
else
  log "Installiere Rust via rustup …"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  ok "Rust installiert — neue Shell öffnen oder: source \"\$HOME/.cargo/env\""
fi

# --- 3) Node-Abhängigkeiten -------------------------------------------------
if [ -d node_modules ]; then
  ok "node_modules vorhanden (bei Bedarf 'npm install' erneut ausführen)."
else
  log "Installiere Node-Abhängigkeiten …"
  npm install
fi

echo
ok "Fertig. Start:  npm run app:dev   (erster cargo-Build dauert ein paar Minuten)"
