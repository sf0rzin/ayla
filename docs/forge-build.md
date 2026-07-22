# Building Ayla on Forge

Forge is the Windows 11 build workstation for Ayla. A private poller checks `origin/main` every two minutes, runs the frontend and Rust validation suite for each new commit, and creates the Tauri Windows executable and NSIS installer. The repository-scoped GitHub Actions runner is also installed, but GitHub currently rejects even minimal workflows with an account-level `startup_failure`; the direct Forge path is the active build path.

## Normal use

1. Develop and validate locally with `npm ci` and `npm run check`.
2. Commit and push to `main`.
3. Allow up to two minutes for Forge to detect the commit and start the build.
4. Retrieve the successful output from `C:\Builds\Artifacts\Ayla\<commit-sha>` on Forge.

To request a build immediately or retry the current commit, run from the Rindexx project:

```powershell
& '.\infra\nyx\forge\Invoke-AylaBuild.ps1' -Ref main
```

## Build behavior

- Source access: a read-only repository deploy key stored only on Forge.
- Automatic trigger: the Windows scheduled task `Rindexx-Ayla-Build-Poller`, every two minutes.
- Direct trigger: `infra/nyx/forge/Invoke-AylaBuild.ps1` in the private Rindexx project.
- GitHub runner: `forge-ayla`, scoped only to `sf0rzin/Ayla`, currently reserved for manual dispatch while the GitHub Actions account issue is unresolved.
- Validation: `npm run check`.
- Packaging: `npm run tauri -- build --bundles nsis`.
- Persistent direct-build Rust cache: `C:\actions-cache\Ayla\direct-target` on Forge.
- Outputs: `C:\Builds\Artifacts\Ayla\<commit-sha>`.
- Logs: `C:\ProgramData\Rindexx\logs\ayla-build-*.log` and `ayla-poller.log`.

The persistent target directory makes repeat Rust builds substantially faster. It is a cache, not a backup, and can be deleted when diagnosing stale build output; the next workflow run recreates it.

## Troubleshooting for an agent

If a pushed commit is not built, use the direct command first. Then check the scheduled task and poller log in the Rindexx infrastructure task. The poller attempts a commit only once; a manual direct build is the approved retry path.

The GitHub Actions runner can still be inspected without changing it:

```powershell
gh api repos/sf0rzin/Ayla/actions/runners --jq '.runners[] | {name,status,busy,labels:[.labels[].name]}'
```

If the runner is offline, hand the issue to the Rindexx infrastructure task. The approved checks there are VM 100 state, QEMU Guest Agent health, the Windows service `actions.runner.sf0rzin-Ayla.forge-ayla`, accurate UTC time, and outbound HTTPS from Forge. Do not expose RDP publicly and do not create a second runner as a shortcut.

If compilation fails, inspect the failed log, reproduce with the same locked dependencies, and fix the project. Avoid clearing the shared cache until the error indicates stale artifacts. The optional workflow pins GitHub-owned actions to immutable commit SHAs; update those pins deliberately and record the corresponding release versions in the comments.
