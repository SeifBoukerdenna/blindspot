---
name: bench
description: Run the blindspot criterion benchmarks and compare query latency against the committed baseline. Use when checking whether a change regressed search or ranking performance.
disable-model-invocation: true
---

# /bench

The entire product is latency. A regression here is invisible until it isn't, so this
runs on demand and reports honestly.

## Steps

1. `cd core && cargo bench --bench query -- --save-baseline current`
2. Compare against the committed baseline:
   `cargo bench --bench query -- --baseline main`
3. Report a table: benchmark name, previous median, current median, delta as a
   percentage.
4. Flag anything where the p99 for a single query exceeds **16ms**. That is one frame
   at 60Hz and it is the number that matters — not the mean.

## Rules

- Do not "fix" a regression in the same run. Report it, then stop and let me decide.
- Do not commit a new baseline unless I explicitly ask. Silently rebaselining destroys
  the only signal this gives us.
- If the benchmark itself fails to build, say so plainly rather than falling back to a
  hand-rolled `std::time::Instant` timing loop. Ad-hoc timings are noise.
- Benchmarks must run against a realistic index size (500+ entries), not three fixtures.
