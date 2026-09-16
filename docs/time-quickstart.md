# Time in Blindspot

Use the launcher’s **Apps** mode and type directly—no `>` or `:content` prefix is needed.

## Check the current time

```text
time in Algiers
time in Paris
Sydney time
what time is it in New York?
```

Blindspot shows the destination’s time, timezone/UTC offset, and time difference from your Mac’s local timezone. Select a result and press **Return** to copy it.

## Convert a time

| Type | Meaning |
| --- | --- |
| `9am to London` | What time is it in London when it is 9 AM where I am? |
| `9am in London` | What time is it where I am when it is 9 AM in London? |
| `3pm Montreal to Tokyo` | Convert 3 PM in Montreal to Tokyo time. |
| `15:00 UTC in Montreal` | Convert 15:00 UTC to Montreal time. |

**`to` uses your local time as the source; `in` uses the named place as the source**, unless you explicitly name both places. Conversion results show both clocks and their difference.

Times can use `3pm`, `3:30pm`, `15:00`, `15h30`, `noon`, or `midnight`. Conversions use today’s date in the source timezone; entering a specific date is not supported by this parser.

## Cities work more reliably than countries

Blindspot uses macOS timezone city names and a limited set of aliases—not a complete country directory.

- Use `time in Algiers`, not `time in Algeria`: the Algeria alias is currently missing.
- Use `time in Paris`, not `time in Paris France`.
- An unrecognized place falls back to ordinary search, which can show unrelated files.

Time calculations happen locally using macOS timezone data, including daylight-saving rules. They do not need AI, embeddings, or a document index.
