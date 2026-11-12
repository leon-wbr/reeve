// A heavy job that needs memory: it holds `mb` of RAM for `seconds` (job.json), as a build or an
// e2e run would. With `passes` set it instead works through its memory that many times (a hash
// of the whole buffer each), so it takes longer when the machine pages its memory out. A short
// job (an id like `w1-s2`) does `shortPasses` when that is set. It writes
// its start and finish to the ledger the bench reads, and keeps a live file while it holds the
// memory, so the bench can tell a job that was killed.
import crypto from 'node:crypto';
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

// Random bytes: macOS compresses a buffer of one repeated byte to almost nothing.
const hold = crypto.randomFillSync(Buffer.allocUnsafe(cfg.mb * 1024 * 1024));
const done = (extra) => {
  log({ event: 'done', bytes: hold.length, ...extra });
  fs.rmSync(mine, { force: true });
  console.log(`job ${id}: done`);
};
const passes = /-s/.test(id) && cfg.shortPasses > 0 ? cfg.shortPasses : cfg.passes;
if (passes > 0) {
  const t0 = Date.now();
  for (let p = 0; p < passes; p++) crypto.createHash('sha1').update(hold).digest();
  done({ workMs: Date.now() - t0 });
} else {
  setTimeout(() => done({}), cfg.seconds * 1000);
}
