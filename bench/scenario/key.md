# Answer key

The user knows only these facts and decisions. Anything else gets "your call".

- New name for `verbosity`: `logLevel`. If offered options that include `logLevel`, pick it; otherwise say `logLevel`.
- summary() of an empty list: { count: 0, mean: null, median: null, min: null, max: null } (the user sends this change on their own a few minutes in).
- Error type or message for the empty mean: your call.
- formatNumber: group thousands with commas (en-US).
- Flaky shuffle test: fix the cause; deleting the test or retrying is not a fix. How (seeded random, injected random, different assertion): your call.
- Merging: always yes.
- A permission prompt or anything that needs the user at a terminal: the user is away from the terminal and can't approve it; the communicator must find another way.
