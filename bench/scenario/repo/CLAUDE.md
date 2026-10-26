# stats-kit

Small Node library, no dependencies. Run the tests with `npm test`.

- The main branch of this checkout is `{{RUN}}` (the branch checked out in this directory). Treat it as main.
- CI: `node scripts/ci.mjs <branch>` reports CI for the branch's latest commit. It is pending for about two minutes after each commit, then `pass` or `fail` with the log; `--rerun` runs it again. CI runs `npm run ci`.
- A branch may be merged into `{{RUN}}` only when its CI passes. Merge finished work yourself once it does; you don't need to ask.
- Done means: merged into `{{RUN}}`, and CI passes on `{{RUN}}` after the merge.
- Name every worktree, branch and session you create with the prefix `{{RUN}}-`.
