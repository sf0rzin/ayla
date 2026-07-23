import { loadConfig } from "./config.js";
import { createDatabase, migrate } from "./db.js";
import { normalizeEmail } from "./validation.js";

const [, , command, emailArgument, confirmation] = process.argv;
const allowedCommands = new Set(["pending", "activate", "disable", "delete"]);

if (
  !allowedCommands.has(command) ||
  (command !== "pending" && !emailArgument) ||
  (command === "delete" && confirmation !== "--confirm")
) {
  console.error(
    "Usage: node server/src/admin.js pending|activate <email>|disable <email>|delete <email> --confirm",
  );
  process.exit(2);
}

const database = createDatabase(loadConfig().database);

try {
  await migrate(database);

  if (command === "pending") {
    const result = await database.query(
      `SELECT name, email, created_at
       FROM users WHERE status = 'pending'
       ORDER BY created_at ASC`,
    );
    if (!result.rowCount) {
      console.log("No pending accounts.");
    } else {
      console.table(result.rows);
    }
  } else if (command === "delete") {
    const email = normalizeEmail(emailArgument);
    const result = await database.query(
      "DELETE FROM users WHERE email = $1 RETURNING id, name, email",
      [email],
    );
    if (!result.rowCount) {
      console.error("Account not found.");
      process.exitCode = 1;
    } else {
      console.table(result.rows);
    }
  } else {
    const email = normalizeEmail(emailArgument);
    const nextStatus = command === "activate" ? "active" : "disabled";
    const result = await database.query(
      `UPDATE users
       SET status = $1::varchar,
           activated_at = CASE
             WHEN $1::varchar = 'active' THEN COALESCE(activated_at, now())
             ELSE activated_at
           END
       WHERE email = $2
       RETURNING id, name, email, status`,
      [nextStatus, email],
    );

    if (!result.rowCount) {
      console.error("Account not found.");
      process.exitCode = 1;
    } else {
      console.table(result.rows);
    }
  }
} finally {
  await database.close();
}
