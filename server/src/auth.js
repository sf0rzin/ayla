import { createHash, randomBytes, randomUUID } from "node:crypto";
import { hashPassword, verifyPassword } from "./password.js";
import { validateLogin, validateRegistration } from "./validation.js";

const SESSION_LIMIT = 5;
const DUMMY_PASSWORD = "not-a-real-account-password";

function tokenHash(token) {
  return createHash("sha256").update(token).digest("hex");
}

function publicUser(row) {
  return {
    id: row.id,
    name: row.name,
    email: row.email,
    role: row.role,
  };
}

function clientMetadata(request) {
  return {
    ipAddress: String(request.ip || "").slice(0, 64) || null,
    userAgent: String(request.headers["user-agent"] || "").slice(0, 300) || null,
  };
}

export async function persistSession(database, session) {
  await database.transaction(async (client) => {
    // Serialize session pruning and insertion for this account. Without this
    // row lock, concurrent successful logins can all observe the same session
    // count and exceed SESSION_LIMIT after committing.
    await client.query("SELECT id FROM users WHERE id = $1 FOR UPDATE", [session.userId]);
    await client.query(
      `UPDATE sessions
       SET revoked_at = COALESCE(revoked_at, now())
       WHERE user_id = $1
         AND revoked_at IS NULL
         AND id NOT IN (
           SELECT id FROM sessions
           WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()
           ORDER BY created_at DESC
           LIMIT $2
         )`,
      [session.userId, SESSION_LIMIT - 1],
    );
    await client.query(
      `INSERT INTO sessions
         (id, user_id, token_hash, expires_at, ip_address, user_agent)
       VALUES ($1, $2, $3, $4, $5, $6)`,
      [
        session.id,
        session.userId,
        session.tokenDigest,
        session.expiresAt,
        session.ipAddress,
        session.userAgent,
      ],
    );
    await client.query("UPDATE users SET last_login_at = now() WHERE id = $1", [session.userId]);
  });
}

export async function createAuthService(database, sessionTtlDays) {
  const dummyHash = await hashPassword(DUMMY_PASSWORD);

  return {
    async register(body) {
      const parsed = validateRegistration(body);
      if (parsed.error) return { ok: false, statusCode: 400, code: parsed.error };

      const passwordHash = await hashPassword(parsed.value.password);
      await database.query(
        `INSERT INTO users (id, name, email, password_hash)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (email) DO NOTHING`,
        [randomUUID(), parsed.value.name, parsed.value.email, passwordHash],
      );

      return { ok: true, statusCode: 202, status: "pending" };
    },

    async login(body, request) {
      const parsed = validateLogin(body);
      if (parsed.error) {
        await verifyPassword(DUMMY_PASSWORD, dummyHash);
        return { ok: false, statusCode: 401, code: "INVALID_CREDENTIALS" };
      }

      const result = await database.query(
        `SELECT id, name, email, password_hash, status, role
         FROM users
         WHERE email = $1`,
        [parsed.value.email],
      );
      const user = result.rows[0];
      const passwordMatches = await verifyPassword(
        parsed.value.password,
        user?.password_hash ?? dummyHash,
      );

      if (!user || !passwordMatches) {
        return { ok: false, statusCode: 401, code: "INVALID_CREDENTIALS" };
      }
      if (user.status === "pending") {
        return { ok: false, statusCode: 403, code: "ACCOUNT_PENDING" };
      }
      if (user.status !== "active") {
        return { ok: false, statusCode: 403, code: "ACCOUNT_DISABLED" };
      }

      const rawToken = randomBytes(32).toString("base64url");
      const sessionId = randomUUID();
      const metadata = clientMetadata(request);
      const expiresAt = new Date(Date.now() + sessionTtlDays * 86_400_000);

      await persistSession(database, {
        id: sessionId,
        userId: user.id,
        tokenDigest: tokenHash(rawToken),
        expiresAt,
        ipAddress: metadata.ipAddress,
        userAgent: metadata.userAgent,
      });

      return {
        ok: true,
        statusCode: 200,
        token: rawToken,
        expiresAt: expiresAt.toISOString(),
        user: publicUser(user),
      };
    },

    async authenticate(authorization) {
      const match = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(authorization ?? "");
      if (!match) return null;

      const result = await database.query(
        `UPDATE sessions AS session
         SET last_seen_at = now()
         FROM users AS account
         WHERE session.token_hash = $1
           AND session.user_id = account.id
           AND session.revoked_at IS NULL
           AND session.expires_at > now()
           AND account.status = 'active'
         RETURNING session.id AS session_id, account.id, account.name,
                   account.email, account.role`,
        [tokenHash(match[1])],
      );
      const row = result.rows[0];
      return row ? { sessionId: row.session_id, user: publicUser(row) } : null;
    },

    async logout(authorization) {
      const match = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(authorization ?? "");
      if (!match) return;
      await database.query(
        `UPDATE sessions SET revoked_at = COALESCE(revoked_at, now())
         WHERE token_hash = $1`,
        [tokenHash(match[1])],
      );
    },

    async cleanupExpiredSessions() {
      await database.query(
        "DELETE FROM sessions WHERE expires_at < now() OR revoked_at < now() - interval '7 days'",
      );
    },
  };
}
