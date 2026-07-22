# Building Ayla on Forge

Forge is the Windows 11 build workstation for Ayla. GitHub Actions checks out the repository there, runs the frontend and Rust validation suite, creates the Tauri Windows executable and NSIS installer, and uploads both as a workflow artifact.

## Normal use

1. Develop and validate locally with `npm ci` and `npm run check`.
2. Commit and push to `main`.
3. Watch the `Forge build` workflow in GitHub or with the commands in `AGENTS.md`.
4. Download the `ayla-windows-<commit>` artifact from the successful run. Artifacts are retained for 14 days.

To request a build without a new commit:

```powershell
gh workflow run forge-build.yml --repo sf0rzin/Ayla --ref main
```

## Build behavior

- Runner: `forge-ayla`, scoped only to `sf0rzin/Ayla`.
- Required labels: `self-hosted`, `Windows`, `X64`, `forge`, `ayla`, `rust`, and `tauri`.
- Trigger policy: trusted pushes to `main` and authenticated manual dispatches only.
- Validation: `npm run check`.
- Packaging: `npm run tauri -- build --bundles nsis`.
- Persistent Rust build cache: `C:\actions-cache\Ayla\target` on Forge.
- Uploaded outputs: the release executable and NSIS installer.

The persistent target directory makes repeat Rust builds substantially faster. It is a cache, not a backup, and can be deleted when diagnosing stale build output; the next workflow run recreates it.

## Troubleshooting for an agent

If a run stays queued, check the repository runner status before changing the workflow:

```powershell
gh api repos/sf0rzin/Ayla/actions/runners --jq '.runners[] | {name,status,busy,labels:[.labels[].name]}'
```

If the runner is offline, hand the issue to the Rindexx infrastructure task. The approved checks there are VM 100 state, QEMU Guest Agent health, the Windows service `actions.runner.sf0rzin-Ayla.forge-ayla`, and outbound HTTPS from Forge. Do not expose RDP publicly and do not create a second runner as a shortcut.

If compilation fails, inspect the failed log, reproduce with the same locked dependencies, and fix the project. Avoid clearing the shared cache until the error indicates stale artifacts. The workflow pins GitHub-owned actions to immutable commit SHAs; update those pins deliberately and record the corresponding release versions in the comments.
