# Ayla authentication API

The development API is served at `https://yl.xyne.gg/api/v1`. PostgreSQL and
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
   stored in PostgreSQL, and sessions expire after 30 days. Login transactions
   lock that user's row while pruning and inserting sessions, so concurrent
   logins cannot exceed the five-session limit.
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
- Caddy accepts public traffic only through Cloudflare, replaces the incoming
  `X-Forwarded-For` chain with `CF-Connecting-IP`, and reaches the API over
  loopback. Fastify trusts only that immediate loopback proxy.
- The API connects as `ayla_app`, a login role without superuser, role-creation,
  database-creation, replication, row-security-bypass, schema-creation, or
  object-ownership privileges. The `database-init` one-shot service runs schema
  migrations as the administrative `ayla` role before the API starts, retains
  ownership of application objects, then grants `ayla_app` only the explicit
  table DML needed by the API. The administrative role remains reserved for
  migrations, maintenance, and backups.
- CORS accepts only the Ayla development and Tauri origins configured in
  `deploy/ayla/compose.yaml`.
- The desktop client keeps the bearer token in React memory only. Closing or
  reloading the app requires a new login.
- `/opt/ayla/.env` remains guest-only and must never be copied into source,
  documentation, shell output, or application logs.

## Operations

Health checks:

```bash
curl https://yl.xyne.gg/api/v1/health
sudo docker compose -f /opt/ayla/compose.yaml --project-directory /opt/ayla ps
```

The versioned application source lives under `/opt/ayla/releases/`, with
`/opt/ayla/app` pointing to the active release. Deployment-time copies of the
previous Compose and Caddy configurations live under `/opt/ayla/rollback/`.
These complement the Ruby rollback sources described by the infrastructure
handoff.

`/opt/ayla/.env` must define distinct, randomly generated values for both
`POSTGRES_PASSWORD` (bootstrap/maintenance) and `POSTGRES_APP_PASSWORD`
(runtime API), and remain mode `0600`. Before the first deployment containing
the runtime role split, add `POSTGRES_APP_PASSWORD` to the existing file. The
idempotent `database-init` service safely creates the role, migrates the schema,
reclaims any legacy application-table ownership for the administrative role,
and reapplies narrow DML grants; PostgreSQL's volume is not recreated.
Deployments must run `docker compose run --rm database-init` before recreating
the API. `AYLA_SCHEMA_VERSION` in Compose is also bumped with every schema
change so an ordinary Compose reconciliation cannot reuse a stale completed
initializer.
