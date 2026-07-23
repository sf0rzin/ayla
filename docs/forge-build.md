# Building Ayla on Forge

Forge is the Windows 11 build workstation for Ayla. A private watcher checks `origin/main` about every 15 seconds, runs the frontend and Rust validation suite for each new commit, and creates the Tauri Windows executable and NSIS installer. The repository-scoped GitHub Actions runner is also installed, but GitHub currently rejects even minimal workflows with an account-level `startup_failure`; the direct Forge path is the active build path.

## Normal use

1. Develop and validate locally with `npm ci` and `npm run check`.
2. Commit and push to `main`.
3. Allow about 15 seconds for Forge to detect the commit and start the build.
4. Retrieve the successful output from `C:\Builds\Artifacts\Ayla\<commit-sha>` on Forge.

To request a build immediately or retry the current commit, run from the Rindexx project:

```powershell
& '.\infra\nyx\forge\Invoke-AylaBuild.ps1' -Ref main
```

## Build behavior

- Source access: a read-only repository deploy key stored only on Forge.
- Automatic trigger: the resident Windows scheduled task `Rindexx-Ayla-Build-Watcher`, polling about every 15 seconds.
- Direct trigger: `infra/nyx/forge/Invoke-AylaBuild.ps1` in the private Rindexx project.
- GitHub runner: `forge-ayla`, scoped only to `sf0rzin/Ayla`, currently reserved for manual dispatch while the GitHub Actions account issue is unresolved.
- Validation: release-profile Rust tests followed by Tauri's single TypeScript/Vite build.
- Packaging: `npm run tauri -- build --bundles nsis`.
- Persistent direct-build Rust cache: `C:\actions-cache\Ayla\direct-target` on Forge.
- Outputs: `C:\Builds\Artifacts\Ayla\<commit-sha>`.
- Logs: `C:\ProgramData\Rindexx\logs\ayla-build-*.log` and `ayla-poller.log`.

The persistent target directory and release-profile tests let the test/package stages reuse Rust output. `npm ci` is skipped when both `package-lock.json` and the existing `node_modules` match. These are caches, not backups, and can be deleted when diagnosing stale build output; the next build recreates them.

Observed on the first fully warm optimized run on 2026-07-22: Forge detected the push in 6.7 seconds and completed tests plus the Windows executable and NSIS installer in 14.5 seconds. Changes that invalidate more Rust dependencies will take longer; cold caches still require a full rebuild.

## Signed updater releases

Ayla source remains in the private `sf0rzin/Ayla` repository. Public updater artifacts are published separately in [`sf0rzin/ayla-releases`](https://github.com/sf0rzin/ayla-releases); its releases contain only signed Windows installers, their detached signatures, and updater metadata. Installed applications read the public channel at:

```text
https://github.com/sf0rzin/ayla-releases/releases/latest/download/latest.json
```

Every updater release must use a stable SemVer version greater than the current published release. The requested version must exactly match `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and the NSIS `ProductVersion`. An installer without Windows version metadata is refused.

### Normal builds and release signing

The repository's base Tauri configuration intentionally does not set `bundle.createUpdaterArtifacts` to `true`. Normal Forge watcher builds therefore remain unsigned updater builds and do not require access to updater signing material. They are useful for validation and internal installation, but they must not be published to the updater channel.

A release build must produce this matching pair:

```text
Ayla_<version>_x64-setup.exe
Ayla_<version>_x64-setup.exe.sig
```

Tauri updater signatures are mandatory. The release job may sign the completed NSIS installer separately with Tauri's signer, or use an explicit release-only Tauri config override that enables updater artifacts. In both cases, the private key and password must be injected only into that transient release process and removed in a `finally` path. They must remain outside both repositories, persistent Forge configuration, application bundles, command output, and build logs.

The publishing helper never reads or manages signing credentials. Before any GitHub mutation, it loads the exact base64-wrapped updater public key from `src-tauri/tauri.conf.json`, decodes both Tauri's Base64 signature envelope and the complete Minisign records inside it, and uses the locked Rust verifier in `tools/update-verifier` to cryptographically verify the installer and detached signature. It also checks all project versions, requires the matching NSIS product version, and checks the current anonymous public release. `GH_DEBUG` must be unset.

### Validate and publish

From a trusted release environment that can access the Forge artifact directory, first validate the metadata without changing GitHub:

```powershell
& '.\scripts\Publish-AylaRelease.ps1' `
  -Version '0.2.0' `
  -ArtifactDirectory 'C:\Builds\Artifacts\Ayla\<commit-sha>' `
  -DryRun
```

The dry run selects the exact versioned NSIS installer and matching `.sig` by name, safely ignoring stale installers from Forge's shared bundle cache. It verifies the signature, confirms the new version is strictly newer than the current published version, computes the installer SHA-256, and writes UTF-8 `latest.json` with an immutable release URL. It may perform anonymous network reads, but it does not create or modify a GitHub release.

After reviewing the version, SHA-256, signature source, and metadata, omit `-DryRun` to publish:

```powershell
& '.\scripts\Publish-AylaRelease.ps1' `
  -Version '0.2.0' `
  -ArtifactDirectory 'C:\Builds\Artifacts\Ayla\<commit-sha>'
```

The helper creates a new draft and includes the installer SHA-256 in its release notes. It uploads the installer, signature, and `latest.json` without `--clobber`, checks GitHub's asset names, sizes, states, and available digests, downloads all three assets into a temporary directory, and compares their SHA-256 hashes with the local files. Only then does it publish the draft and mark it as the latest release. After publication, it performs unauthenticated HTTPS requests for the public `latest.json` and installer, validates the version, literal signature, immutable URL, size, and installer SHA-256, and retries briefly for GitHub propagation.

### One-time bootstrap verification

The first updater-aware Ayla build is a one-time bootstrap and must be installed manually. Before running it:

1. Download the installer and its `.sig` from the same public release.
2. Compare the local hash with the `Installer SHA-256` value in that release's notes:

   ```powershell
   Get-FileHash -LiteralPath '.\Ayla_0.2.0_x64-setup.exe' -Algorithm SHA256
   ```

3. Verify the detached signature against the exact public key embedded in the private source checkout:

   ```powershell
   $config = Get-Content -LiteralPath '.\src-tauri\tauri.conf.json' -Raw | ConvertFrom-Json
   $pubkey = $config.plugins.updater.pubkey
   cargo run --quiet --locked `
     --manifest-path '.\tools\update-verifier\Cargo.toml' -- `
     '.\Ayla_0.2.0_x64-setup.exe' `
     '.\Ayla_0.2.0_x64-setup.exe.sig' `
     $pubkey
   ```

The verifier must print `signature verified`. Starting with that installed version, later releases can be discovered and installed from Ayla's update controls.

### Draft and publication recovery

Preflight signature or version failures occur before any GitHub mutation. If creation succeeds but a later prepublication step fails, inspect the draft before doing anything else:

- An empty draft for the exact same tag can be retried explicitly with `-ResumeDraft`. The helper requires exactly one matching draft and refuses published or non-empty releases.
- A partially uploaded draft cannot be resumed or clobbered. Inspect its assets, then remove the bad draft and tag manually only after confirming the version and cause; start a fresh publication afterward.
- If the final anonymous verification fails, the release may already be public. Confirm the tag, immediately quarantine it back to a draft, and investigate before recreating the release:

  ```powershell
  gh release edit 'v0.2.0' --repo 'sf0rzin/ayla-releases' --draft
  ```

Never repair a release with `--clobber`; updater metadata, signature, and installer must remain one immutable set.

## Troubleshooting for an agent

If a pushed commit is not built, use the direct command first. Then check the scheduled task and poller log in the Rindexx infrastructure task. The poller attempts a commit only once; a manual direct build is the approved retry path.

The GitHub Actions runner can still be inspected without changing it:

```powershell
gh api repos/sf0rzin/Ayla/actions/runners --jq '.runners[] | {name,status,busy,labels:[.labels[].name]}'
```

If the runner is offline, hand the issue to the Rindexx infrastructure task. The approved checks there are VM 100 state, QEMU Guest Agent health, the Windows service `actions.runner.sf0rzin-Ayla.forge-ayla`, accurate UTC time, and outbound HTTPS from Forge. Do not expose RDP publicly and do not create a second runner as a shortcut.

If compilation fails, inspect the failed log, reproduce with the same locked dependencies, and fix the project. Avoid clearing the shared cache until the error indicates stale artifacts. The optional workflow pins GitHub-owned actions to immutable commit SHAs; update those pins deliberately and record the corresponding release versions in the comments.
