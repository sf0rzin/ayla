import assert from "node:assert/strict";
import test from "node:test";
import { hashPassword, verifyPassword } from "../src/password.js";

test("password hashes verify only the original password", async () => {
  const hash = await hashPassword("Ayla-test-password!42");
  assert.equal(await verifyPassword("Ayla-test-password!42", hash), true);
  assert.equal(await verifyPassword("wrong-password", hash), false);
  assert.equal(hash.includes("Ayla-test-password!42"), false);
});

test("malformed password hashes are rejected", async () => {
  assert.equal(await verifyPassword("anything", "not-a-password-hash"), false);
});
