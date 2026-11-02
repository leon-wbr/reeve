// Rebuilds test/fixtures/sales.json from the shared test database. It holds the database for
// about six minutes (test/db.json's rebuildSeconds), so nothing else can use it meanwhile.
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { config, withDatabase } from './db.mjs';

export const VERSION = 2;

// The rows the database holds for 2026: one sale per day, refunds on every 9th.
export function sales() {
  const rows = [];
  let x = 20260101;
  for (let day = 0; day < 365; day++) {
    x = (x * 1103515245 + 12345) % 2147483648;
    const date = new Date(Date.UTC(2026, 0, 1 + day)).toISOString().slice(0, 10);
    const cents = 500 + (x % 9500);
    rows.push({ date, cents: day % 9 === 8 ? -cents : cents, country: ['DE', 'FR', 'AT'][x % 3] });
  }
  return { version: VERSION, rows };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const out = join(dirname(fileURLToPath(import.meta.url)), '../test/fixtures/sales.json');
  console.log(`rebuilding ${out} from the test database, about ${Math.round(config.rebuildSeconds / 60)} min...`);
  await withDatabase('rebuild', config.rebuildSeconds, () => {
    writeFileSync(out, JSON.stringify(sales(), null, 1) + '\n');
  });
  console.log('done');
}
