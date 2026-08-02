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

## Module coverage

| Module | Status | Validation |
| --- | --- | --- |
| ChatGPT | Available | Authenticated session and plan classification |
| Twitch | Preview | Authentication, Prime, Turbo, and role classification |
| HBO Max | Preview | Authentication, entitlement, and subscription-state classification |

Twitch and HBO Max have isolated adapters and synthetic test coverage. Verification with dedicated live test accounts is still pending. The other twelve catalog modules remain intentionally disabled until their adapters are implemented and tested.

## Core capabilities

- Bounded recursive discovery with configurable directory, file, and scan limits.
- Cancellable background tasks with controlled concurrency, retries, delays, and aggregate-only history.
- Proxy import, normalization, deduplication, persistence, and concurrent health checks.
- Direct connections and user-provided HTTP, SOCKS4a, and SOCKS5h proxy routes.
- Module-scoped cookie parsing with domain, path, expiry, size, and complexity checks.
- Optional result export that preserves the original artifact and classifies a copy by outcome.

## Chromium companion extension

`extensions/ayla-cookie-manager` contains a Manifest V3 cookie manager for
Chrome, Edge, Brave, and other Chromium browsers. It follows Ayla's visual
language and supports cookie inspection, creation, editing, deletion,
protection, JSON/Netscape backup and restore, LocalStorage cleanup, and
partitioned cookies. See the extension's [installation and security notes](extensions/ayla-cookie-manager/README.md).

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
- Node.js and npm
- Stable Rust with the MSVC target
- Visual Studio C++ Build Tools and WebView2
- CMake, NASM, and libclang

`LIBCLANG_PATH` must point to the directory containing `libclang.dll` when LLVM is not available on `PATH`.

### Run locally

```powershell
npm ci
npm run tauri dev
```

### Validate

```powershell
npm run check
```

`npm run check` is the canonical validation command and runs the production
frontend build, authentication API tests, and the Rust test suite.

## Account service

The desktop login and registration flow uses the development API at
`https://ayla.rindexx.cc/api/v1`. New accounts remain pending until an operator
activates them. The API source, container deployment, endpoint contract, and
administrator commands are documented in
[Ayla authentication API](docs/auth-api.md).

## Windows builds

Release packages are produced on the dedicated Forge Windows environment. See [Building Ayla on Forge](docs/forge-build.md) for the trusted build and artifact workflow.

Signed application updates and the authentication API are served from
`ayla.rindexx.cc`; see [Self-hosted Ayla services](docs/self-hosted-updater.md)
for deployment, backup, and release publication details.
