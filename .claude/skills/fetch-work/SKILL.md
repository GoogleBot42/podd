---
name: fetch-work
description: Use when Jeremy wants agent work pulled from the backlog instead of specifying a task outright — "what should I work on", "find me something small", "pick something and go", "give me some options". Surfaces ranked candidate work from issues/docs/TODOs; not for executing a task he's already named.
---

# fetch-work

Pull ranked candidate work for this session. Read
`.claude/skills/fetch-work/sources.md` first — it owns the list of where work
comes from; query every source listed there each time (don't rely on a cached
mental list, the README and issue tracker move).

## Modes

Infer which mode from Jeremy's phrasing; ask only if genuinely ambiguous.

**(a) Filtered — he names a kind of work** ("ui work", "something small",
"safety stuff"). Query all sources, filter to matches, present the filtered
ranked list. Don't silently drop the filter if nothing matches — say so and
offer the closest alternatives.

**(b) Agent-choose** ("pick something", "just find something and start").
Query all sources, rank (below), pick the single highest-value item, state a
one-line justification, then start on it.

**(c) Suggest** (default when unspecified, or "give me options"/"suggest
something"). Query all sources, rank, present a shortlist (3-6 items) with
effort estimates, and wait for Jeremy to pick.

## Procedure

1. Read `sources.md`, run each source's query.
2. Dedupe — an open issue and a README "remaining work" item are often the
   same underlying task; a TODO comment may be the concrete half of a
   REPLACEMENT_PLAN gap. Merge, don't list twice.
3. Rank by, in order: Jeremy's stated priority (mode a/explicit asks) >
   safety-tripwire items (anything touching the actuation path per root
   `CLAUDE.md`) > items that unblock other work (see the `unblock` skill's
   framing — but this skill ranks agent work, not human actions) > lower
   effort first among ties.
4. Output a short ranked list, each item with a one-line source citation
   (e.g. "Gitea #14", "README remaining-work", "REPLACEMENT_PLAN §9 gap",
   "TODO in crates/pod-proto/src/sensor/packet.rs:205", "memory: pending
   decision on X").

Keep the output short — a scannable list, not a report. Don't pad with
items you wouldn't actually recommend just to hit a target count.
