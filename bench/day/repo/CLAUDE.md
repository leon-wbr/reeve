# shop-kit

Small Node shop library, no dependencies.

- The main branch of this checkout is `{{RUN}}` (the branch checked out in this directory). Treat it as main.
- Tests: `npm test` runs everything, about a minute, because the integration suite uses the shared test database. A single file, such as `node --test test/unit.test.js`, takes a second.
- The shared test database serves every checkout on this machine. Only one job can use it at a time: `npm test` or `node scripts/rebuild-fixtures.mjs`. A second one fails with "test database busy".
- Merge a branch into `{{RUN}}` only after `npm test` passes on it. Merge finished work yourself; you don't need to ask.
- Done means: merged into `{{RUN}}`, with `npm test` passing there.
- Name every worktree, branch and session you create with the prefix `{{RUN}}-`.
- Don't edit `test/db.json`.
