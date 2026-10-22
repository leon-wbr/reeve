// Fake CI: `node scripts/ci.mjs <branch> [--rerun]` reports CI for the branch's latest commit.
// A run starts when the commit is made (or on --rerun) and finishes about two minutes later.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const delay = Number(process.env.CI_DELAY_S ?? 120);
const [branch, flag] = process.argv.slice(2);
if (!branch) { console.error('usage: node scripts/ci.mjs <branch> [--rerun]'); process.exit(64); }
const git = (...a) => execFileSync('git', a, { encoding: 'utf8' }).trim();
const sha = git('rev-parse', `${branch}^{commit}`);
const store = join(resolve(git('rev-parse', '--git-common-dir')), 'ci');
mkdirSync(store, { recursive: true });
const file = join(store, `${sha}.json`);
let rec = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : { startedAt: Number(git('show', '-s', '--format=%ct', sha)) * 1000 };
if (flag === '--rerun') rec = { startedAt: Date.now() };
const left = Math.ceil((rec.startedAt + delay * 1000 - Date.now()) / 1000);
if (!rec.status && left > 0) {
  writeFileSync(file, JSON.stringify(rec));
  console.log(`CI ${sha.slice(0, 7)} on ${branch}: pending, about ${left}s left`);
  process.exit(2);
}
if (!rec.status) {
  const dir = mkdtempSync(join(tmpdir(), 'ci-'));
  try {
    git('archive', '-o', join(dir, 'snap.tar'), sha);
    execFileSync('tar', ['-xf', 'snap.tar'], { cwd: dir });
    execFileSync('npm', ['run', '-s', 'ci'], { cwd: dir, stdio: 'pipe' });
    rec.status = 'pass';
  } catch (e) {
    rec.status = 'fail';
    rec.log = `${e.stdout ?? ''}${e.stderr ?? ''}`.split('\n').slice(-40).join('\n');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  writeFileSync(file, JSON.stringify(rec));
}
console.log(`CI ${sha.slice(0, 7)} on ${branch}: ${rec.status}`);
if (rec.log) console.log(rec.log);
process.exit(rec.status === 'pass' ? 0 : 1);
