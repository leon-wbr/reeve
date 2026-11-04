// The long job: about 4 minutes, then writes out/job.txt in the current directory.
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
const code = process.argv[2] || "none";
const seconds = Number(process.env.JOB_SECONDS || "{{JOB_SECONDS}}");
appendFileSync("{{LEDGER}}", `job-start ${code} ${Date.now()} ${process.pid} ${process.cwd()}\n`);
setTimeout(() => {
  mkdirSync("out", { recursive: true });
  writeFileSync("out/job.txt", `job result ${code}\n`);
  appendFileSync("{{LEDGER}}", `job-finish ${code} ${Date.now()}\n`);
  console.log(`job ${code} finished: out/job.txt`);
}, seconds * 1000);
