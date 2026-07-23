import assert from "node:assert/strict";
import test from "node:test";
import { normalizeEmail, validateLogin, validateRegistration } from "../src/validation.js";

test("registration normalizes safe account fields", () => {
  assert.deepEqual(
    validateRegistration({
      name: "  Preview   User  ",
      email: " PREVIEW@EXAMPLE.COM ",
      password: "Ayla-test-password!42",
    }),
    {
      value: {
        name: "Preview User",
        email: "preview@example.com",
        password: "Ayla-test-password!42",
      },
    },
  );
});

test("registration rejects weak or malformed input", () => {
  assert.equal(validateRegistration({}).error, "INVALID_NAME");
  assert.equal(
    validateRegistration({ name: "User", email: "invalid", password: "long-enough-password" }).error,
    "INVALID_EMAIL",
  );
  assert.equal(
    validateRegistration({ name: "User", email: "user@example.com", password: "short" }).error,
    "INVALID_PASSWORD",
  );
});

test("login uses a normalized email and generic invalid-credential errors", () => {
  assert.equal(normalizeEmail(" USER@EXAMPLE.COM "), "user@example.com");
  assert.deepEqual(
    validateLogin({ email: " USER@EXAMPLE.COM ", password: "password" }),
    { value: { email: "user@example.com", password: "password" } },
  );
  assert.equal(validateLogin({ email: "bad", password: "" }).error, "INVALID_CREDENTIALS");
});
