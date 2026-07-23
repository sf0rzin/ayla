import pg from "pg";

const { Pool } = pg;

export function createDatabase(connection) {
  const pool = new Pool({
    ...connection,
    max: 10,
    idleTimeoutMillis: 30_000,
    connectionTimeoutMillis: 5_000,
  });

  return {
    query: (text, values) => pool.query(text, values),
    async transaction(callback) {
      const client = await pool.connect();
      try {
        await client.query("BEGIN");
        const result = await callback(client);
        await client.query("COMMIT");
        return result;
      } catch (error) {
        await client.query("ROLLBACK");
        throw error;
      } finally {
        client.release();
      }
    },
    close: () => pool.end(),
  };
}

export async function migrate(database) {
  await database.query(`
    CREATE TABLE IF NOT EXISTS users (
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
      activated_by uuid REFERENCES users(id),
      last_login_at timestamptz
    );

    CREATE TABLE IF NOT EXISTS sessions (
      id uuid PRIMARY KEY,
      user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
      token_hash char(64) NOT NULL UNIQUE,
      created_at timestamptz NOT NULL DEFAULT now(),
      expires_at timestamptz NOT NULL,
      revoked_at timestamptz,
      last_seen_at timestamptz NOT NULL DEFAULT now(),
      ip_address varchar(64),
      user_agent varchar(300)
    );

    CREATE INDEX IF NOT EXISTS sessions_user_id_idx ON sessions(user_id);
    CREATE INDEX IF NOT EXISTS sessions_expiry_idx ON sessions(expires_at)
      WHERE revoked_at IS NULL;
  `);
}
