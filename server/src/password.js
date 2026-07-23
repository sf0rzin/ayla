import { randomBytes, scrypt as scryptCallback, timingSafeEqual } from "node:crypto";
import { promisify } from "node:util";

const scrypt = promisify(scryptCallback);
const KEY_LENGTH = 64;
const COST = 131_072;
const BLOCK_SIZE = 8;
const PARALLELIZATION = 1;
const MAX_MEMORY = 256 * 1024 * 1024;

export async function hashPassword(password) {
  const salt = randomBytes(16);
  const derived = await scrypt(password, salt, KEY_LENGTH, {
    N: COST,
    r: BLOCK_SIZE,
    p: PARALLELIZATION,
    maxmem: MAX_MEMORY,
  });

  return [
    "scrypt",
    COST,
    BLOCK_SIZE,
    PARALLELIZATION,
    salt.toString("base64url"),
    Buffer.from(derived).toString("base64url"),
  ].join("$");
}

export async function verifyPassword(password, storedHash) {
  const [algorithm, costText, blockSizeText, parallelizationText, saltText, hashText] =
    String(storedHash).split("$");
  const cost = Number.parseInt(costText, 10);
  const blockSize = Number.parseInt(blockSizeText, 10);
  const parallelization = Number.parseInt(parallelizationText, 10);

  if (
    algorithm !== "scrypt" ||
    cost !== COST ||
    blockSize !== BLOCK_SIZE ||
    parallelization !== PARALLELIZATION ||
    !saltText ||
    !hashText
  ) {
    return false;
  }

  try {
    const salt = Buffer.from(saltText, "base64url");
    const expected = Buffer.from(hashText, "base64url");
    if (salt.length !== 16 || expected.length !== KEY_LENGTH) return false;

    const actual = Buffer.from(
      await scrypt(password, salt, expected.length, {
        N: cost,
        r: blockSize,
        p: parallelization,
        maxmem: MAX_MEMORY,
      }),
    );
    return timingSafeEqual(actual, expected);
  } catch {
    return false;
  }
}
