# Releasing & self-updates

BooN Transfer ships **signed self-updates** via Tauri's updater, independent of
any distro. Installed **AppImage** builds check a GitHub Release on startup and
update themselves. `.deb`/`.rpm` installs update through the package manager /
distro repo as usual.

## How it fits together

- `tauri.conf.json` → `plugins.updater`: the **public key** and the endpoint
  `https://github.com/BoondockSulfur/BooNTransfer/releases/latest/download/latest.json`.
- `bundle.createUpdaterArtifacts: true`: each bundle build also emits a signed
  `.sig`. **A build that bundles now requires the signing key** (see below).
- On launch the app calls the updater; if the release advertises a newer
  version, a banner offers *Install & restart*. The download is verified against
  the public key before it is applied.

## The signing key

Generated once with `tauri signer generate`:

- **Private key:** `~/.tauri/boon-transfer-updater.key` — **secret, never
  committed.** Losing the key *or* its password means you can no longer sign
  updates (installed apps would reject them). Back both up.
- **Public key:** embedded in `tauri.conf.json` (safe to commit).

### GitHub Actions secrets (required for releases)

In the repo → Settings → Secrets and variables → Actions:

| Secret | Value |
|--------|-------|
| `TAURI_SIGNING_PRIVATE_KEY` | the **contents** of `~/.tauri/boon-transfer-updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the key's password |

## Cutting a release

1. Bump the version in **three** files (keep them in sync): `package.json`,
   `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`.
2. Commit, then tag and push:
   ```sh
   git commit -am "release: v0.1.1"
   git tag v0.1.1
   git push origin main --tags
   ```
3. `.github/workflows/release.yml` (tag-triggered) builds the signed bundles,
   creates the GitHub Release, and uploads the artifacts **plus `latest.json`**
   (the updater manifest). Installed AppImages pick it up on their next launch.

## Building signed bundles locally

Because `createUpdaterArtifacts` is on, a local `npm run app:build` needs the
key in the environment:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/boon-transfer-updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD='<password>'
npm run app:build
```

Each `bundle/appimage/*.AppImage` then has a matching `.sig`.

## Rotating the key

Generate a new keypair (`tauri signer generate`), replace `plugins.updater.pubkey`
in `tauri.conf.json`, update the two Actions secrets, and release. Note that
apps installed with the **old** key will not accept updates signed by the new
one — plan a rotation as a clean-install boundary.
