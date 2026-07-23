const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function normalizeEmail(value) {
  return typeof value === "string" ? value.trim().toLowerCase() : "";
}

export function validateRegistration(body) {
  if (!isPlainObject(body)) return { error: "INVALID_REQUEST" };

  const name = typeof body.name === "string" ? body.name.trim().replace(/\s+/g, " ") : "";
  const email = normalizeEmail(body.email);
  const password = typeof body.password === "string" ? body.password : "";

  if (name.length < 2 || name.length > 80) return { error: "INVALID_NAME" };
  if (email.length > 254 || !EMAIL_PATTERN.test(email)) return { error: "INVALID_EMAIL" };
  if (password.length < 12 || password.length > 128) return { error: "INVALID_PASSWORD" };

  return { value: { name, email, password } };
}

export function validateLogin(body) {
  if (!isPlainObject(body)) return { error: "INVALID_REQUEST" };

  const email = normalizeEmail(body.email);
  const password = typeof body.password === "string" ? body.password : "";
  if (email.length > 254 || !EMAIL_PATTERN.test(email) || !password || password.length > 128) {
    return { error: "INVALID_CREDENTIALS" };
  }

  return { value: { email, password } };
}
