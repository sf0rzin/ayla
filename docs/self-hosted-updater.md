# Self-hosted Ayla services

The production service is available at `https://ayla.rindexx.cc` and contains
two independent public surfaces:

- `/api/v1/*` proxies to the Fastify authentication API on guest loopback.
- `/updates/*` serves signed, immutable Tauri updater artifacts.

PostgreSQL is attached only to the internal Docker network. Caddy is the only
public HTTP service, and the guest firewall accepts web traffic only from the
published Cloudflare address ranges. The updater signing private key must never
be copied to this VM.

## VM layout

```text
/opt/ayla/                         application deployment
/opt/ayla/.env                    root-only PostgreSQL password
/srv/ayla-public/updates/         public updater channel
/var/backups/ayla/                root-only logical database backups
/etc/caddy/Caddyfile              HTTPS and routing configuration
/etc/nftables.conf                guest firewall configuration
```

The deployed services use `deploy/ayla/compose.yaml`. Database backups run from
`ayla-db-backup.timer`, validate every custom-format dump with `pg_restore
--list`, and retain fourteen days locally. Proxmox or off-host backups must copy
those dumps outside the guest; local retention is not an off-site backup.

## Build a signed update

Keep the updater key and password in the trusted Windows release environment.
After synchronizing the stable SemVer in `package.json`, `src-tauri/Cargo.toml`,
and `src-tauri/tauri.conf.json`, build with the release-only override:

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = '<injected outside the repository>'
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = '<injected outside the repository>'

npm run tauri -- build `
  --config src-tauri/tauri.release.conf.json `
  --bundles nsis
```

The artifact directory must contain the exact pair:

```text
Ayla_<version>_x64-setup.exe
Ayla_<version>_x64-setup.exe.sig
```

## Publish

The self-hosted publisher verifies all project versions, Windows product
metadata, and the Tauri signature before it changes the server. It uploads into
a private staging directory, creates an immutable version directory, publishes
`latest.json` last, and downloads the public installer to compare its size and
SHA-256.

```powershell
& '.\scripts\Publish-AylaSelfHostedRelease.ps1' `
  -Version '0.3.2' `
  -ArtifactDirectory '.\src-tauri\target\release\bundle\nsis' `
  -DryRun

& '.\scripts\Publish-AylaSelfHostedRelease.ps1' `
  -Version '0.3.2' `
  -ArtifactDirectory '.\src-tauri\target\release\bundle\nsis'
```

Never replace files inside a published version directory. If a release is bad,
publish a higher SemVer after correcting it. Until a signed release exists, the
stable endpoint intentionally returns HTTP `204 No Content`.

## Operations

```bash
curl https://ayla.rindexx.cc/api/v1/health
curl -i https://ayla.rindexx.cc/updates/stable/latest.json
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml ps
sudo systemctl status caddy nftables ayla-db-backup.timer
sudo systemctl start ayla-db-backup.service
```

Cloudflare proxy ranges appear in both the Caddy trusted-proxy configuration and
the guest firewall. Refresh both lists together from Cloudflare's published IPv4
and IPv6 range endpoints.
