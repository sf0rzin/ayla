# Ayla

Desktop application built with Rust, Tauri 2, and React/TypeScript. The project is being rebuilt incrementally, keeping the Rust core separate from the interface so the design can be replaced entirely.

## Current state

- Tauri 2 desktop shell.
- Catalog of 14 modules, with ChatGPT and Twitch available in the current MVP.
- Local settings validated and persisted by the backend.
- Rust proxy parser with deduplication and support for HTTP(S), SOCKS4, and SOCKS5.
- Proxy manager with import, persistence, removal, and clearing.
- Concurrent checking with real-time progress, timeout, and cancellation.
- Rust task engine running in the background, with aggregated progress, selective cancellation, and a safe history.
- ChatGPT MVP: local artifact filtering, authenticated session/plan validation, and optional proxy.
- Twitch MVP: bounded Netscape/JSON parsing, authenticated GraphQL validation, Prime/Turbo/role classification, browser-compatible TLS transport, retry/rate limiting, and optional HTTP/SOCKS proxies with a bounded lazy client cache for large pools.
- Module-neutral summaries and result export into `<module>/active` and `<module>/failed` without modifying the source files.
- `Tasks` page to prepare runs, follow the active work, and review previous summaries.
- Desktop interface based on the Grafite DS provided by the user.
- Custom window bar, three-pane navigation, Geist, and local Lucide icons.
- Unit tests and simulated local servers for HTTP, SOCKS4a, and SOCKS5.

Proxies that do not respond are removed after the check, following the behavior of the reference project. Each proxy update is now O(1), with a single persistence at the end of the round, work distribution by atomic cursor, a bounded channel, and a total per-proxy timeout.

The engine accepts one global run at a time in this stage. Before starting, it removes blanks and duplicates in O(n) while preserving order, and limits the run to 10,000 unique files, 20,000 raw lines, 32 KiB per line, 32 MiB of paths, 512 MiB of artifacts, and 32 workers. Paths exist only in the IPC payload and in the run's transient memory: they never enter events, logs, or history, and are not persisted.

The ChatGPT and Twitch adapters read at most 2 MiB per file, validate domain, path, expiration, values, JSON complexity, and cookie count, and then confirm the session against their authenticated endpoints. Clients preserve timeout, retries, concurrency, cancellation, and active HTTP/SOCKS proxies. Paths, cookies, tokens, and account identifiers do not enter events, logs, or history; the interface receives only aggregated counts.

## Development

Prerequisites: Node.js, stable Rust with the MSVC target, Visual Studio C++ Build Tools, WebView2, CMake, NASM, and libclang. The last three are required by the BoringSSL transport used for browser-compatible TLS on Windows; `LIBCLANG_PATH` must point to the directory containing `libclang.dll` when LLVM is not on `PATH`.

```powershell
npm install
npm run tauri dev
```

Full validation:

```powershell
npm run check
```

## Structure

```text
src/                    React desktop interface
src-tauri/src/catalog.rs module catalog
src-tauri/src/auth_artifact.rs local parser and structural ChatGPT classification
src-tauri/src/cookie_artifact.rs bounded module-scoped cookie parser
src-tauri/src/module_probe.rs module-neutral probe results and plan labels
src-tauri/src/twitch_client.rs authenticated Twitch transport and classification
src-tauri/src/proxy.rs   proxy parser and normalization
src-tauri/src/proxy_store.rs list persistence and operations
src-tauri/src/proxy_checker.rs concurrent checking and protocols
src-tauri/src/task_engine.rs task engine, progress, cancellation, and history
src-tauri/src/settings.rs settings and persistence
src-tauri/src/lib.rs     commands exposed to the interface
```

## Interface

The components are written in React and wired to the Rust backend; the original HTML prototype is not needed at runtime.

## Security

Cookies, sessions, licenses, real proxies, results, and `tdata` folders must not be added to the repository. The normal suite uses only synthetic data. External examples stay outside the project: the app reads only explicitly provided paths and the ignored test requires opt-in; both return only aggregated totals. The task history stores summaries exclusively: no entry, path, or credential is recorded.

## Next steps

1. Migrate the next modules individually, keeping each integration isolated and tested.
2. Authentication/licensing with secure storage.
3. Evolve scheduling toward multiple global runs, when needed.
4. Isolated migration of Telegram `tdata` support.
