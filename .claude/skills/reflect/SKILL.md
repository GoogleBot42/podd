---
name: reflect
description: Use at the end of a work session, or when Jeremy asks to "reflect", "update the setup from this session", or "clean up what we learned" — reviews friction from this session and patches CLAUDE.md/rules/skills/memory if warranted. Not for mid-task work; run once things are wrapping up.
---

# reflect

**"No changes needed" is the expected, common outcome of this skill.** Most
sessions don't surface anything worth codifying. Do not invent changes just to
justify having run it — a session that ends with "reviewed, nothing to
change" is a success, not a failure to find something.

## Steps

1. **Review this session's friction.** Look for: guidance that turned out
   stale or wrong and had to be corrected mid-session; a command or fact that
   was re-derived because nothing documented it (path, invocation, forge
   command, etc.); a fact that was looked up more than once because it wasn't
   written down anywhere durable.

2. **Patch stale lines, don't add new pointers.** Root `CLAUDE.md`'s "Docs
   map" section states one owner per fact — when something is wrong, find and
   fix it *at its owner*, not by adding a correction or caveat somewhere else
   that points back at it. Two exceptions to "one owner": lines explicitly
   marked "deliberate duplication — keep" (currently: the sensor-MCU zombie
   window, duplicated between `CLAUDE.md` and `.claude/rules/actuation-safety.md`)
   are intentional and must both be kept in sync, not collapsed to one.

3. **Extend before creating.** If an existing skill already covers the area
   where friction occurred, extend it — add the missing command, correct the
   stale claim, add a short section. Only propose a *new* skill if a
   procedure was re-derived from scratch 2+ times this session (or is known
   from memory/prior sessions to have been re-derived repeatedly) and no
   existing skill's scope reasonably covers it. Avoid near-duplicate skills;
   check `.claude/skills/*/SKILL.md` descriptions first.

4. **Prune the memory index.**
   `~/.claude/projects/-home-googlebot-workspace-eightsleep-podd/memory/MEMORY.md`
   is for transient state — pending decisions, in-flight statuses. Once an
   entry has been codified into `CLAUDE.md`, a rule, a skill, or a doc, it no
   longer needs to live in memory in full: replace it with a one-line
   "codified in `<file>`" pointer, or delete it outright if the pointer adds
   nothing. Don't let memory accumulate permanent facts that now live
   elsewhere — that's the duplication this step exists to catch.

## Output

State up front whether anything changed. If nothing did, say so in the first
line and stop — no need to narrate the review process at length. If something
did change, list each file touched and the one-line reason.
