# Ayla authentication API

The development API is served at `https://ayla.rindexx.cc/api/v1`. PostgreSQL and
the API run in Docker on VMID 103. PostgreSQL is attached only to the internal
Docker network; the API is published on guest loopback at `127.0.0.1:3000`, and
Caddy exposes only `/api/*` through the existing Ruby ingress.

## Account flow

1. `POST /auth/register` validates the account, hashes the password with scrypt,
   and creates a `pending` user. The response is always `202` for a syntactically
   valid registration, including an already registered email.
2. An operator reviews and activates the account from the guest:

   ```bash
   cd /opt/ayla
   sudo docker compose exec -T api node server/src/admin.js pending
   sudo docker compose exec -T api node server/src/admin.js activate user@example.com
   ```

3. `POST /auth/login` returns `ACCOUNT_PENDING` until activation. An active user
   receives an opaque 256-bit bearer token. Only the SHA-256 token digest is
   stored in PostgreSQL, and sessions expire after 30 days.
4. `GET /auth/me` validates the bearer token. `POST /auth/logout` revokes it.

Operators can disable an account with `admin.js disable <email>`. Permanent
deletion requires the explicit `admin.js delete <email> --confirm` command and
also removes that account's sessions.

## Security boundaries

- Passwords, bearer tokens, authorization headers, and request bodies are not
  logged.
- Passwords must contain 12–128 characters and are stored using scrypt with a
  per-password random salt.
- Login and registration have stricter per-IP rate limits than the global API.
- CORS accepts only the Ayla development and Tauri origins configured in
  `deploy/ayla/compose.yaml`.
- The desktop client keeps the bearer token in React memory only. Closing or
  reloading the app requires a new login.
- `/opt/ayla/.env` remains guest-only and must never be copied into source,
  documentation, shell output, or application logs.

## Operations

Health checks:

```bash
curl https://ayla.rindexx.cc/api/v1/health
sudo docker compose -f /opt/ayla/compose.yaml --project-directory /opt/ayla ps
```

The versioned application source lives under `/opt/ayla/releases/`, with
`/opt/ayla/app` pointing to the active release. Deployment-time copies of the
previous Compose and Caddy configurations live under `/opt/ayla/rollback/`.
These complement the Ruby rollback sources described by the infrastructure
handoff.
