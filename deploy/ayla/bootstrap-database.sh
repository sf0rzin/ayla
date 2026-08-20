#!/bin/sh
set -eu

: "${PGHOST:?PGHOST is required}"
: "${PGDATABASE:?PGDATABASE is required}"
: "${PGUSER:?PGUSER is required}"
: "${PGPASSWORD:?PGPASSWORD is required}"
: "${POSTGRES_APP_PASSWORD:?POSTGRES_APP_PASSWORD is required}"

# This idempotent bootstrap runs as the cluster administrator before the API.
# It handles both a new volume and the existing installation whose tables were
# originally owned by the `ayla` bootstrap superuser.
psql \
	--set ON_ERROR_STOP=1 \
	--set app_password="$POSTGRES_APP_PASSWORD" \
	--dbname "$PGDATABASE" \
	--username "$PGUSER" <<'SQL'
SELECT format(
  'CREATE ROLE ayla_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS PASSWORD %L',
  :'app_password'
)
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ayla_app')
\gexec

SELECT format(
  'ALTER ROLE ayla_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS PASSWORD %L',
  :'app_password'
)
\gexec

GRANT CONNECT ON DATABASE ayla TO ayla_app;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM ayla_app;

CREATE TABLE IF NOT EXISTS public.users (
  id uuid PRIMARY KEY,
  name varchar(80) NOT NULL,
  email varchar(254) NOT NULL UNIQUE,
  password_hash text NOT NULL,
  status varchar(16) NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'active', 'disabled')),
  role varchar(16) NOT NULL DEFAULT 'user'
    CHECK (role IN ('user', 'admin')),
  created_at timestamptz NOT NULL DEFAULT now(),
  activated_at timestamptz,
  activated_by uuid REFERENCES public.users(id),
  last_login_at timestamptz
);

CREATE TABLE IF NOT EXISTS public.sessions (
  id uuid PRIMARY KEY,
  user_id uuid NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
  token_hash char(64) NOT NULL UNIQUE,
  created_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz,
  last_seen_at timestamptz NOT NULL DEFAULT now(),
  ip_address varchar(64),
  user_agent varchar(300)
);

CREATE INDEX IF NOT EXISTS sessions_user_id_idx ON public.sessions(user_id);
CREATE INDEX IF NOT EXISTS sessions_expiry_idx ON public.sessions(expires_at)
  WHERE revoked_at IS NULL;

DO $bootstrap$
BEGIN
  IF to_regclass('public.users') IS NOT NULL THEN
    ALTER TABLE public.users OWNER TO ayla;
  END IF;
  IF to_regclass('public.sessions') IS NOT NULL THEN
    ALTER TABLE public.sessions OWNER TO ayla;
  END IF;
END
$bootstrap$;

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM ayla_app;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM ayla_app;
GRANT USAGE ON SCHEMA public TO ayla_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.users, public.sessions TO ayla_app;

-- Future migrations run as ayla in this one-shot service. They do not silently
-- broaden runtime access; each new object needs an explicit least-privilege grant.
ALTER DEFAULT PRIVILEGES FOR ROLE ayla IN SCHEMA public REVOKE ALL ON TABLES FROM ayla_app;
ALTER DEFAULT PRIVILEGES FOR ROLE ayla IN SCHEMA public REVOKE ALL ON SEQUENCES FROM ayla_app;
SQL
