// A heavy job that needs memory: it holds `mb` of RAM for `seconds` (job.json), as a build or an
// e2e run would. It writes its start and finish to the ledger the bench reads, and keeps a live
// file while it holds the memory, so the bench can tell a job that was killed.
import fs from 'node:fs';
import path from 'node:path';

const i = process.argv.indexOf('--id');
const id = i > 0 ? process.argv[i + 1] : `pid-${process.pid}`;
const cfg = JSON.parse(fs.readFileSync(new URL('../job.json', import.meta.url), 'utf8'));
const live = path.join(cfg.ledger, 'live');
fs.mkdirSync(live, { recursive: true });
const log = (e) =>
  fs.appendFileSync(
    path.join(cfg.ledger, 'events.jsonl'),
    JSON.stringify({ t: Date.now(), id, pid: process.pid, mb: cfg.mb, ...e }) + '\n',
  );
const mine = path.join(live, `${process.pid}.json`);
fs.writeFileSync(mine, JSON.stringify({ pid: process.pid, id, mb: cfg.mb }));
log({ event: 'start' });

const hold = Buffer.alloc(cfg.mb * 1024 * 1024, 1);
setTimeout(() => {
  log({ event: 'done', bytes: hold.length });
  fs.rmSync(mine, { force: true });
  console.log(`job ${id}: done`);
}, cfg.seconds * 1000);
