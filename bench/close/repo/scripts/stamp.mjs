// Prints a stamp code for a worker and records it where the bench can see it.
import { appendFileSync } from "node:fs";
import { randomBytes } from "node:crypto";
const name = process.argv[2] || "unnamed";
const code = "STAMP-" + randomBytes(4).toString("hex");
appendFileSync("{{LEDGER}}", `stamp ${name} ${code} ${Date.now()}\n`);
console.log(code);
