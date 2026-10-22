Six things for stats-kit:
1. mean() of an empty list returns NaN. It should throw a clear error instead.
2. Add a median() to the stats module, with tests.
3. The config key `verbosity` is a bad name. Rename it, but check the new name with me first.
4. Tests are red on main. Please fix that.
5. Add summary(xs) to stats, returning { count, mean, median, min, max }, with tests.
6. The shuffle tests fail now and then. Fix the flakiness properly, no retries.
Merge each one when it's ready.
