# Ayla agent guide

Before changing Ayla, read `README.md` and `docs/forge-build.md`. Never commit cookies, sessions, tokens, proxies, account material, `.env` files, `tdata`, or generated results.

The canonical validation command is `npm run check`. Windows release builds run on Forge; do not assume that a local Linux or macOS build proves the Tauri Windows package works.

A push to `main` is normally detected by the Forge watcher within about 15 seconds and built automatically. To request or retry a build immediately, run this from the private Rindexx project:

```powershell
& '.\infra\nyx\forge\Invoke-AylaBuild.ps1' -Ref main
```

The command returns the built commit SHA, artifact directory, and log path. The automatic poller attempts each commit once; use the command above for an explicit retry after a transient failure. Never paste credentials or private application data into logs.

The repository-scoped GitHub Actions workflow remains available for manual dispatch after the account-level Actions startup failure is resolved. It executes trusted repository code as the Forge development user. Do not add `pull_request`, `pull_request_target`, fork, or arbitrary-ref execution without an explicit security review. Do not modify the poller, deploy key, runner registration, or Forge services from this repository; that infrastructure is documented in the private Rindexx project.
