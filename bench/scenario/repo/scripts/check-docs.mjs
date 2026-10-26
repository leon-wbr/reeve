// Every function exported from src/ must be documented in README.md as `name(`.
import { readdirSync, readFileSync } from 'node:fs';

const readme = readFileSync('README.md', 'utf8');
const missing = [];
for (const f of readdirSync('src')) {
  for (const [, name] of readFileSync(`src/${f}`, 'utf8').matchAll(/export function (\w+)/g)) {
    if (!readme.includes(`${name}(`)) missing.push(`${name} (src/${f})`);
  }
}
if (missing.length) {
  console.error(`check-docs: not documented in README.md: ${missing.join(', ')}`);
  process.exit(1);
}
