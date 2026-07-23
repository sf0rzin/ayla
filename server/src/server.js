import Fastify from "fastify";
import cors from "@fastify/cors";
import helmet from "@fastify/helmet";
import rateLimit from "@fastify/rate-limit";
import { pathToFileURL } from "node:url";
import { createAuthService } from "./auth.js";
import { loadConfig } from "./config.js";
import { createDatabase, migrate } from "./db.js";

export async function buildServer(config = loadConfig()) {
  const app = Fastify({
    bodyLimit: 16 * 1024,
    trustProxy: true,
    logger: {
      level: config.logLevel,
      redact: ["req.headers.authorization", "headers.authorization"],
    },
  });
  const database = createDatabase(config.database);
  await migrate(database);
  const auth = await createAuthService(database, config.sessionTtlDays);

  await app.register(helmet, {
    contentSecurityPolicy: false,
  });
  await app.register(cors, {
    credentials: false,
    methods: ["GET", "POST", "OPTIONS"],
    origin(origin, callback) {
      if (!origin || config.corsOrigins.has(origin)) {
        callback(null, true);
        return;
      }
      callback(null, false);
    },
  });
  await app.register(rateLimit, {
    global: true,
    max: 120,
    timeWindow: "1 minute",
  });

  app.get("/api/v1/health", async () => {
    await database.query("SELECT 1");
    return { status: "ok" };
  });

  app.post(
    "/api/v1/auth/register",
    { config: { rateLimit: { max: 3, timeWindow: "1 hour" } } },
    async (request, reply) => {
      const result = await auth.register(request.body);
      if (!result.ok) {
        return reply.code(result.statusCode).send({ error: { code: result.code } });
      }
      return reply.code(result.statusCode).send({
        status: result.status,
        message: "Registration received. An administrator must activate the account.",
      });
    },
  );

  app.post(
    "/api/v1/auth/login",
    { config: { rateLimit: { max: 5, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const result = await auth.login(request.body, request);
      if (!result.ok) {
        return reply.code(result.statusCode).send({ error: { code: result.code } });
      }
      return reply.send({
        token: result.token,
        expiresAt: result.expiresAt,
        user: result.user,
      });
    },
  );

  app.get("/api/v1/auth/me", async (request, reply) => {
    const session = await auth.authenticate(request.headers.authorization);
    if (!session) {
      return reply.code(401).send({ error: { code: "UNAUTHORIZED" } });
    }
    return { user: session.user };
  });

  app.post("/api/v1/auth/logout", async (request, reply) => {
    await auth.logout(request.headers.authorization);
    return reply.code(204).send();
  });

  app.setNotFoundHandler((_request, reply) =>
    reply.code(404).send({ error: { code: "NOT_FOUND" } }),
  );
  app.setErrorHandler((error, request, reply) => {
    request.log.error({ err: error }, "request failed");
    const statusCode =
      error.statusCode && error.statusCode >= 400 && error.statusCode < 500
        ? error.statusCode
        : 500;
    const code =
      statusCode === 429
        ? "RATE_LIMITED"
        : statusCode < 500
          ? "INVALID_REQUEST"
          : "INTERNAL_ERROR";
    reply.code(statusCode).send({ error: { code } });
  });

  const cleanupTimer = setInterval(() => {
    auth.cleanupExpiredSessions().catch((error) => app.log.error({ err: error }, "session cleanup failed"));
  }, 60 * 60 * 1000);
  cleanupTimer.unref();

  app.addHook("onClose", async () => {
    clearInterval(cleanupTimer);
    await database.close();
  });

  return app;
}

async function start() {
  const config = loadConfig();
  const app = await buildServer(config);

  const shutdown = async (signal) => {
    app.log.info({ signal }, "shutting down");
    await app.close();
    process.exit(0);
  };
  process.once("SIGINT", () => void shutdown("SIGINT"));
  process.once("SIGTERM", () => void shutdown("SIGTERM"));

  await app.listen({ host: config.host, port: config.port });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  start().catch((error) => {
    console.error("Ayla API failed to start:", error.message);
    process.exit(1);
  });
}
