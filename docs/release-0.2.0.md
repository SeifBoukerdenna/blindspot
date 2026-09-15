# Blindspot 0.2.0 delivery

Installed and launched on September 14, 2026:
`~/Applications/Blindspot.app`, verified process PID 18820 and bundle version 0.2.0.

Downloads: `~/Downloads/Blindspot-0.2.0.zip` and
`~/Downloads/Blindspot-0.2.0-Cheatsheet.md`.
ZIP SHA-256: `d4e938f8402660edfd338d92cdfa3cdcf17fdea5462112e8fbe9b9f5b034ddac`.

Release app build and Developer ID signature verification passed, including all three
helpers. Focused action/parser checks, clipboard retention (1), command registry (3),
and process tests (7, including the native socket check outside the restricted sandbox)
passed. The local model service responded with five installed models. Full tests, scale
benchmarks and broad manual testing were intentionally not repeated at the user's request.
Computer Use permission was unavailable, so visual window verification was not performed.

The final coding/release-resource tasks used gpt-5.6-luna with Astra coordination/review,
as requested. See new-features.md for supported commands and current limitations;
roadmap-status.md contains historical engineering checkpoints. This local signed package
is not notarized and is not a claim that every original roadmap feature is implemented.
