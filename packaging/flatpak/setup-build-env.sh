#!/usr/bin/env bash
# One-time setup to build the BooN Transfer Flatpak locally.
# Installs flatpak + flatpak-builder and the GNOME runtime/SDK the manifest uses.
# Safe, standard packages. Run once:  ! bash packaging/flatpak/setup-build-env.sh
set -euo pipefail

RUNTIME_VERSION=49

echo "==> Installing flatpak + flatpak-builder (needs sudo)…"
if command -v apt-get >/dev/null; then
  sudo apt-get update
  sudo apt-get install -y flatpak flatpak-builder
elif command -v dnf >/dev/null; then
  sudo dnf install -y flatpak flatpak-builder
elif command -v pacman >/dev/null; then
  sudo pacman -S --needed --noconfirm flatpak flatpak-builder
else
  echo "Unknown package manager — install 'flatpak' and 'flatpak-builder' manually." >&2
  exit 1
fi

echo "==> Adding Flathub remote (user scope)…"
flatpak remote-add --user --if-not-exists flathub \
  https://flathub.org/repo/flathub.flatpakrepo

echo "==> Installing GNOME Platform + SDK ${RUNTIME_VERSION} (a few hundred MB)…"
flatpak install --user -y flathub \
  "org.gnome.Platform//${RUNTIME_VERSION}" \
  "org.gnome.Sdk//${RUNTIME_VERSION}"

echo "==> Done. flatpak-builder is ready."
flatpak-builder --version
