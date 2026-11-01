// The shared test database. Every checkout on this machine uses the same one (test/db.json), and
// only one job may hold it at a time: the integration suite or the fixture rebuild.
import { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
export const config = JSON.parse(readFileSync(join(root, 'test/db.json'), 'utf8'));

const alive = (pid) => {
  try { process.kill(pid, 0); return true; } catch { return false; }
};

function log(entry) {
  mkdirSync(config.path, { recursive: true });
  appendFileSync(join(config.path, 'runs.jsonl'), JSON.stringify({
    ...entry, heavy: process.env.REEVE_HEAVY_JOB === '1', cwd: process.cwd(),
  }) + '\n');
}

// Holds the database for `seconds` while `work` runs. Throws if another job holds it.
export async function withDatabase(what, seconds, work = () => {}) {
  mkdirSync(config.path, { recursive: true });
  const lock = join(config.path, 'lock');
  const start = Date.now();
  try {
    mkdirSync(lock);
  } catch {
    let owner = 0;
    try { owner = Number(readFileSync(join(lock, 'pid'), 'utf8')); } catch {}
    if (owner && alive(owner)) {
      log({ what, start, end: Date.now(), ok: false, busy: true });
      throw new Error(`test database busy: another job (pid ${owner}) is using it, try again when it is done`);
    }
    rmSync(lock, { recursive: true, force: true });
    mkdirSync(lock);
  }
  writeFileSync(join(lock, 'pid'), String(process.pid));
  let ok = false;
  try {
    await new Promise((r) => setTimeout(r, seconds * 1000));
    const out = await work();
    ok = true;
    return out;
  } finally {
    rmSync(lock, { recursive: true, force: true });
    log({ what, start, end: Date.now(), ok, busy: false });
  }
}
