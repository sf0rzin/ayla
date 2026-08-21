# Self-hosted Ayla services

The production service is available at `https://yl.xyne.gg` and contains
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
/opt/ayla/.env                    root-only PostgreSQL admin/runtime passwords
/srv/ayla-public/updates/         public updater channel
/var/backups/ayla/                root-only logical database backups
/etc/caddy/Caddyfile              HTTPS and routing configuration
/etc/nftables.conf                guest firewall configuration
```

The deployed services use `deploy/ayla/compose.yaml`. `/opt/ayla/.env` must
contain different random values for `POSTGRES_PASSWORD` and
`POSTGRES_APP_PASSWORD`. The first remains the PostgreSQL bootstrap/maintenance
credential; the API receives only the non-superuser `ayla_app` credential. An
idempotent one-shot service creates or updates that role before API startup and
runs schema migrations as the administrative role. Existing `users` and
`sessions` ownership is reclaimed by `ayla`; the runtime role receives only
explicit `SELECT`, `INSERT`, `UPDATE`, and `DELETE` grants and cannot create or
own schema objects. The role split does not require recreating the volume.

Database backups run from `ayla-db-backup.timer`. Every custom-format dump is
fully read and decompressed by `pg_restore`, receives a SHA-256 sidecar, and is
promoted only after those checks pass. On Sunday UTC by default, the same job
also restores the dump with `--exit-on-error` into a uniquely named temporary
database, reads both application tables, and drops that database. Set
`AYLA_RESTORE_VERIFY_WEEKDAY` to `1` through `7` in the systemd service to choose
another UTC weekday; `never` disables only the restore drill, not daily full
archive validation. The dump and matching `.sha256` file are retained for
fourteen days. Proxmox or off-host backups must copy and verify both outside the
guest; local retention is not an off-site backup.

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
a private staging directory, takes an exclusive server-side publication lock,
and rechecks that the requested version is newer while holding that lock. It
assembles each immutable version in a nonce-scoped directory and renames the
complete directory atomically, then atomically publishes `latest.json` from a
different nonce-scoped temporary file. A retry after interruption is accepted
only when the installer/signature are byte-for-byte identical and every
metadata field except the necessarily regenerated `pub_date` is unchanged; the
first immutable metadata copy is then reused. Finally it downloads the public
installer to compare its size and SHA-256. The service VM must provide `flock`,
`sha256sum`, `stat`, `cmp`, `awk`, and `sed` from its base system.

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
curl https://yl.xyne.gg/api/v1/health
curl -i https://yl.xyne.gg/updates/stable/latest.json
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml ps
sudo systemctl status caddy nftables ayla-db-backup.timer
sudo systemctl start ayla-db-backup.service
```

Before the first role-split deployment, add `POSTGRES_APP_PASSWORD` to the
existing root-only `.env`. On this and every later deployment, run the
idempotent migration explicitly before recreating `api`:

```bash
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml run --rm database-init
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml up -d api
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml ps -a database-init api
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml exec -T postgres \
  psql -U ayla -d ayla -Atc "SELECT rolname, rolsuper, rolcreatedb, rolcreaterole FROM pg_roles WHERE rolname = 'ayla_app'"
```

Increment `AYLA_SCHEMA_VERSION` in Compose whenever the migration script
changes. This forces Compose to recreate a previously completed initializer as
a second safeguard; it does not replace the explicit `run --rm` deployment
step.

The nftables file destroys and recreates only `table inet ayla_filter` in one
atomic `nft -f` transaction; it never flushes Docker's tables. Validate the
installed nft parser and keep a recovery SSH session before the first reload:

```bash
sudo nft --check --file /etc/nftables.conf
sudo systemctl reload nftables
sudo nft list table inet ayla_filter
sudo docker compose --env-file /opt/ayla/.env -f /opt/ayla/compose.yaml ps
```

Cloudflare proxy ranges appear in both the Caddy trusted-proxy configuration and
the guest firewall. Refresh both lists together from Cloudflare's published IPv4
and IPv6 range endpoints.
