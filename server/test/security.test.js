import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import Fastify from "fastify";
import rateLimit from "@fastify/rate-limit";
import { persistSession } from "../src/auth.js";
import { TRUSTED_PROXY_ADDRESSES } from "../src/server.js";

function immediate() {
  return new Promise((resolve) => setImmediate(resolve));
}

function createSessionDatabase() {
  const sessions = [];
  const lockTails = new Map();
  let sequence = 0;

  async function acquireUserLock(userId) {
    const previous = lockTails.get(userId) ?? Promise.resolve();
    let release;
    const current = new Promise((resolve) => {
      release = resolve;
    });
    lockTails.set(userId, previous.then(() => current));
    await previous;
    return release;
  }

  return {
    sessions,
    async transaction(callback) {
      let releaseUserLock;
      const client = {
        async query(text, values) {
          const normalized = text.replace(/\s+/g, " ").trim();
          if (normalized === "SELECT id FROM users WHERE id = $1 FOR UPDATE") {
            releaseUserLock = await acquireUserLock(values[0]);
            return { rowCount: 1, rows: [{ id: values[0] }] };
          }
          if (normalized.startsWith("UPDATE sessions")) {
            const [userId, keepCount] = values;
            const keep = new Set(
              sessions
                .filter((session) => session.userId === userId && !session.revoked)
                .sort((left, right) => right.sequence - left.sequence)
                .slice(0, keepCount)
                .map((session) => session.id),
            );
            await immediate();
            for (const session of sessions) {
              if (session.userId === userId && !session.revoked && !keep.has(session.id)) {
                session.revoked = true;
              }
            }
            return { rowCount: 0, rows: [] };
          }
          if (normalized.startsWith("INSERT INTO sessions")) {
            await immediate();
            sessions.push({
              id: values[0],
              userId: values[1],
              revoked: false,
              sequence: ++sequence,
            });
            return { rowCount: 1, rows: [] };
          }
          if (normalized.startsWith("UPDATE users SET last_login_at")) {
            return { rowCount: 1, rows: [] };
          }
          throw new Error(`Unexpected test query: ${normalized}`);
        },
      };

      try {
        return await callback(client);
      } finally {
        releaseUserLock?.();
      }
    },
  };
}

test("only the immediate loopback proxy can set request.ip", async (context) => {
  const app = Fastify({ trustProxy: TRUSTED_PROXY_ADDRESSES });
  context.after(() => app.close());
  app.get("/ip", (request) => ({ ip: request.ip }));

  const proxied = await app.inject({
    method: "GET",
    url: "/ip",
    remoteAddress: "127.0.0.1",
    headers: { "x-forwarded-for": "203.0.113.40" },
  });
  assert.equal(proxied.json().ip, "203.0.113.40");

  const direct = await app.inject({
    method: "GET",
    url: "/ip",
    remoteAddress: "198.51.100.20",
    headers: { "x-forwarded-for": "203.0.113.99" },
  });
  assert.equal(direct.json().ip, "198.51.100.20");
});

test("sanitized Cloudflare addresses share one rate-limit bucket", async (context) => {
  const caddyfile = await readFile(new URL("../../deploy/ayla/Caddyfile", import.meta.url), "utf8");
  assert.match(
    caddyfile,
    /header_up\s+X-Forwarded-For\s+\{http\.request\.header\.CF-Connecting-IP\}/,
  );
  assert.doesNotMatch(caddyfile, /client_ip_headers[^\n]*X-Forwarded-For/);

  const app = Fastify({ trustProxy: TRUSTED_PROXY_ADDRESSES });
  context.after(() => app.close());
  await app.register(rateLimit, { global: false });
  app.get(
    "/limited",
    { config: { rateLimit: { max: 1, timeWindow: "1 minute" } } },
    () => ({ ok: true }),
  );

  const request = {
    method: "GET",
    url: "/limited",
    remoteAddress: "127.0.0.1",
    headers: { "x-forwarded-for": "203.0.113.40" },
  };
  assert.equal((await app.inject(request)).statusCode, 200);
  assert.equal((await app.inject(request)).statusCode, 429);
});

test("twenty concurrent session inserts leave at most five active sessions", async () => {
  const database = createSessionDatabase();
  const userId = "00000000-0000-4000-8000-000000000001";
  const expiresAt = new Date(Date.now() + 86_400_000);

  await Promise.all(
    Array.from({ length: 20 }, (_, index) =>
      persistSession(database, {
        id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
        userId,
        tokenDigest: String(index).padStart(64, "0"),
        expiresAt,
        ipAddress: "203.0.113.40",
        userAgent: "Ayla security regression",
      }),
    ),
  );

  assert.equal(database.sessions.filter((session) => !session.revoked).length, 5);
});
