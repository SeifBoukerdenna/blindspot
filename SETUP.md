# blindspot — Claude Code setup

Do these in order. Each step is independently useful; stop wherever you've had enough.

---

## Step 1 — Repo skeleton

```bash
mkdir -p blindspot && cd blindspot
git init
mkdir -p core/src shell include .claude/hooks .claude/rules .claude/skills .claude/agents
cargo init --lib core --name blindspot_core
```

In `core/Cargo.toml`:

```toml
[lib]
crate-type = ["staticlib", "rlib"]
```

The `rlib` matters — without it you can't write unit tests or benches against the crate.

Drop `CLAUDE.md` at the repo root (the one from earlier).

---

## Step 2 — Copy in the `.claude/` directory

Everything in this bundle goes at the repo root. Then:

```bash
chmod +x .claude/hooks/*.sh
```

Commit `.claude/settings.json`, the rules, skills, and agents. Anything machine-specific
goes in `.claude/settings.local.json`, which stays out of git.

Restart your session after adding agent files — subagents load at session start.

---

## Step 3 — Verify the hooks actually fire

Don't trust that config works. Test it:

```bash
echo '{"tool_input":{"command":"git push --force origin main"}}' | bash .claude/hooks/guard-bash.sh; echo "exit=$?"
# expect: BLOCKED on stderr, exit=2

echo '{"tool_input":{"command":"cargo build"}}' | bash .claude/hooks/guard-bash.sh; echo "exit=$?"
# expect: exit=0
```

Exit code 2 blocks the tool call and feeds your stderr message back to Claude, so the
wording of the block message matters — it's an instruction Claude will read and act on.

Then in a session, run `/hooks` to confirm Claude Code registered them.

---

## Step 4 — Understand what you just installed

**`guard-bash.sh`** (PreToolUse on Bash) blocks force pushes, hard resets, recursive
deletes, `tccutil reset`, and any attempt to touch Spotlight's hotkey binding. That last
one is the interesting case: your CLAUDE.md says the Cmd+Space unbind is a manual user
step, but an instruction in CLAUDE.md is a request, not a guarantee. The hook is the
guarantee.

**`guard-paths.sh`** (PreToolUse on writes) blocks `.env`, signing material, build
artifacts, and the cbindgen-generated header.

**`rust-check.sh`** (PostToolUse on writes) formats the file and runs clippy with
warnings as errors. On failure it exits 2, so the errors land back in Claude's context
and it fixes them before moving on instead of at the end of a twenty-file change. It
greps to the first 40 diagnostic lines — an unbounded dump would be worse than nothing.

**`swift-check.sh`** does a `swiftc -parse` syntax gate only. A full `xcodebuild` on
every edit would be far too slow for a per-edit hook.

**`.claude/rules/ffi.md`** and **`appkit.md`** are path-scoped, so they only load when
Claude opens matching files. That keeps CLAUDE.md short while still putting the
memory-safety rules in front of Claude at exactly the moment it edits `ffi.rs`.

**`/bench`** is set `disable-model-invocation: true` — it costs zero context and only
you can fire it. Correct setting for anything with side effects or that you want to
control the timing of.

**`macos-researcher`** has read and search tools only, no write access. It burns
context reading Apple docs in an isolated window and hands back a short answer.

---

## Step 5 — Working rhythm

The config is maybe a third of the value. The loop is the rest.

**Plan before executing anything nontrivial.** Enter plan mode, read the plan yourself,
approve it, then let it run. The failure mode of agentic coding isn't bad syntax — it's
confidently correct code built on a wrong premise, and the plan is the only cheap place
to catch that.

**Delegate the reading, keep the writing.** `Ask macos-researcher what collection
behavior flags I need for a panel that shows over fullscreen apps` costs you three lines
of context instead of forty pages of docs.

**One milestone per session.** Your CLAUDE.md has M1–M5 for a reason. Start a fresh
session at each boundary rather than letting one context window sprawl across the whole
project.

**Run `/context` when things feel off.** Skill descriptions and MCP tool names load
every request; if Claude starts forgetting your conventions, context pressure is the
usual cause before capability is.

---

## Step 6 — Add later, when triggered

Don't build these now. Add each when the specific trigger fires.

| Trigger | Add |
| --- | --- |
| You've typed the same prompt three times | A skill |
| M4 arrives and you're auditing the whole index for correctness | A dynamic workflow (`/config` to enable on Pro) |
| Rust core and Swift shell are both big enough to work on independently | Two sessions in separate git worktrees |
| You start a second Rust+Swift project | Package this `.claude/` as a plugin |

A dynamic workflow is worth it specifically because the script can have independent
agents cross-check each other's findings before you see them — that's a quality
mechanism, not just parallelism. Overkill for M1.

---

## Step 7 — First real prompt

Once it's wired up, open a session and start with something that exercises the whole
setup:

> Read CLAUDE.md. Plan M1 only — global hotkey, panel, fuzzy match over /Applications,
> Enter to launch. Don't write code yet. Flag anything in the plan where you're
> guessing at an AppKit API rather than knowing it, and delegate those to
> macos-researcher before we start.

That last sentence is the one that matters. It converts the model's uncertainty into
research instead of into plausible-looking wrong code.
