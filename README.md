<p align="center">
  <img src="docs/assets/ayla-banner.png" alt="Ayla over an aurora-lit mountain landscape" width="100%" />
</p>

# Ayla

**A Windows desktop application for authorized session validation.**

Ayla brings bounded Rust task execution, service-specific validation, optional proxy routing, and deterministic result export into one local workflow. Its interface is built with React and TypeScript; security-sensitive processing stays in the Tauri 2 backend.

> [!IMPORTANT]
> Ayla is an early-stage MVP. Use it only with accounts, session artifacts, proxies, and systems that you own or are explicitly authorized to test.

## Workflow

1. Select individual artifacts or folders to scan recursively.
2. Ayla filters unsupported and structurally invalid files locally.
3. The selected module validates eligible sessions directly or through the active proxy pool.
4. Aggregated progress is reported in the app, with optional classified copies exported to a directory you choose.

Source artifacts are never modified.

Directory, file-count, and aggregate scan-byte limits can each be set to **Unlimited** independently. Directory traversal starts in Unlimited mode for new or missing settings. In that mode Ayla does not substitute a hidden numeric ceiling: traversal is incremental and prepared artifacts move through a fixed-capacity worker queue, so memory usage stays proportional to directory depth and concurrency instead of the total tree size.

## Module coverage

| Module | Status | Validation |
| --- | --- | --- |
| ChatGPT | Available | Authenticated session and plan classification |
| Twitch | Preview | Authentication, Prime, Turbo, and role classification |
| HBO Max | Preview | Authentication, entitlement, and subscription-state classification |

Twitch and HBO Max have isolated adapters and synthetic test coverage. Verification with dedicated live test accounts is still pending. The other twelve catalog modules remain intentionally disabled until their adapters are implemented and tested.

## Core capabilities

- Streaming recursive discovery with finite or truly unlimited directory, file-count, and aggregate scan-byte limits.
- Cancellable background tasks with controlled concurrency, retries, delays, and aggregate-only history.
- Proxy import, normalization, deduplication, persistence, and concurrent health checks.
- Direct connections and user-provided HTTP, SOCKS4a, and SOCKS5h proxy routes.
- Module-scoped cookie parsing with domain, path, expiry, size, and complexity checks.
- Optional result export that preserves the original artifact and classifies a copy by outcome.

## Chromium companion extension

`extensions/ayla-cookie-manager` contains **Ayla Cookies for Microsoft Edge**, a
Manifest V3 companion extension that also works in modern Chromium browsers. It
follows Ayla's visual language, imports complete Netscape `cookies.txt` or JSON
backups, can remove every cookie in the current browser context after an
explicit confirmation, and supports inspection, editing, protection,
LocalStorage cleanup, and partitioned cookies (CHIPS). See the extension's
[installation and security notes](extensions/ayla-cookie-manager/README.md), or
download the ready-to-upload
[Microsoft Edge package](extensions/Ayla-Cookies-for-Edge-v0.2.0.zip).

## Result layout

When export is enabled, Ayla creates an isolated structure inside the selected directory:

```text
<selected-directory>/
└── <module>/
    ├── active/
    └── failed/
```

Exported filenames contain classification labels and an opaque run identifier. Account identifiers are not used in filenames.

## Privacy and security

- Cookies, sessions, tokens, proxy lists, account material, generated results, `.env` files, and `tdata` directories must remain outside the repository.
- Selected source paths and session material are designed to stay out of application events, logs, and persisted task history; the interface receives aggregate task data.
- Authenticated validation contacts the relevant service endpoints and can use a configured proxy when requested.
- Proxy configuration is stored in the local application data directory. Authentication fields are withheld from interface responses, but the current proxy store is not encrypted at rest; protect the Windows profile accordingly.
- The normal test suite uses synthetic fixtures. Tests requiring authorized external data are opt-in and ignored by default.

## Development

### Requirements

- Windows with the MSVC toolchain; release packages are validated on Forge running Windows 11
- Node.js 24.19.0 and npm 11.17.0 (`.node-version` records the Node release)
- Rust 1.97.1 with the `x86_64-pc-windows-msvc` target (enforced by `rust-toolchain.toml`)
- Visual Studio C++ Build Tools and WebView2
- CMake 4.4.2, NASM 2.16.03, and LLVM/libclang 22.1.8

The native combination above is the reproducible Forge baseline and is known to
compile `btls-sys`. NASM 3.x is not part of the supported baseline.
`LIBCLANG_PATH` must point to the directory containing `libclang.dll` when LLVM
is not available on `PATH`.

### Run locally

```powershell
npm ci
npm run tauri dev
```

To skip the account screen during local development, pass the dedicated
application argument through Tauri:

```powershell
npm run tauri -- dev -- -- --skip-login
```

The argument creates an in-memory local test session and never calls the
authentication API. Both sides are development-gated: the Vite development
frontend requests it only under `tauri dev`, and the Rust backend returns it
only with debug assertions. Production bundles do not contain the command or
test-session markers, and release executables ignore `--skip-login`.

### Validate

```powershell
npm run check
```

`npm run check` is the canonical validation command and runs the production
frontend build, authentication API tests, and the Rust test suite.

## Account service

The desktop login and registration flow uses the development API at
`https://yl.xyne.gg/api/v1`. New accounts remain pending until an operator
activates them. The API source, container deployment, endpoint contract, and
administrator commands are documented in
[Ayla authentication API](docs/auth-api.md).

## Windows builds

Release packages are produced on the dedicated Forge Windows environment. See [Building Ayla on Forge](docs/forge-build.md) for the trusted build and artifact workflow.

Signed application updates and the authentication API are served from
`yl.xyne.gg`; see [Self-hosted Ayla services](docs/self-hosted-updater.md)
for deployment, backup, and release publication details.
