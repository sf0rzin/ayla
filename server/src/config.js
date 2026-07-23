const DEFAULT_ORIGINS = [
  "http://localhost:1420",
  "http://127.0.0.1:1420",
  "http://tauri.localhost",
  "https://tauri.localhost",
  "tauri://localhost",
];

function positiveInteger(value, fallback, name) {
  const parsed = Number.parseInt(value ?? "", 10);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    if (value === undefined) return fallback;
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

export function loadConfig(environment = process.env) {
  const databaseUrl = environment.DATABASE_URL?.trim();
  const database = databaseUrl
    ? { connectionString: databaseUrl }
    : {
        host: environment.PGHOST?.trim(),
        port: positiveInteger(environment.PGPORT, 5432, "PGPORT"),
        database: environment.PGDATABASE?.trim(),
        user: environment.PGUSER?.trim(),
        password: environment.PGPASSWORD,
      };
  if (
    !databaseUrl &&
    (!database.host || !database.database || !database.user || !database.password)
  ) {
    throw new Error("DATABASE_URL or complete PG* connection settings are required");
  }

  const configuredOrigins = environment.CORS_ORIGINS
    ?.split(",")
    .map((origin) => origin.trim())
    .filter(Boolean);

  return {
    database,
    host: environment.API_HOST?.trim() || "0.0.0.0",
    port: positiveInteger(environment.API_PORT, 3000, "API_PORT"),
    sessionTtlDays: positiveInteger(environment.SESSION_TTL_DAYS, 30, "SESSION_TTL_DAYS"),
    corsOrigins: new Set(configuredOrigins?.length ? configuredOrigins : DEFAULT_ORIGINS),
    logLevel: environment.LOG_LEVEL?.trim() || "info",
  };
}
