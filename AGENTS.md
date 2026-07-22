# Ayla agent guide

Before changing Ayla, read `README.md` and `docs/forge-build.md`. Never commit cookies, sessions, tokens, proxies, account material, `.env` files, `tdata`, or generated results.

The canonical validation command is `npm run check`. Windows release builds run on the repository-scoped GitHub Actions runner `forge-ayla`; do not assume that a local Linux or macOS build proves the Tauri Windows package works.

A push to `main` starts `.github/workflows/forge-build.yml`. An agent may also start it manually with:

```powershell
gh workflow run forge-build.yml --repo sf0rzin/Ayla --ref main
```

Follow the run with `gh run list --repo sf0rzin/Ayla --workflow forge-build.yml` and `gh run watch <run-id> --repo sf0rzin/Ayla --exit-status`. If it fails, inspect only the failed steps with `gh run view <run-id> --repo sf0rzin/Ayla --log-failed`; never paste credentials or private application data into logs.

The runner executes trusted repository code as the Forge development user. Do not add `pull_request`, `pull_request_target`, fork, or arbitrary-ref execution without an explicit security review. Do not modify runner registration or the Forge service from this repository; that infrastructure is documented in the private Rindexx project.
