---
name: unblock
description: Use when Jeremy asks what he needs to do himself, what's blocked on him, or wants a punch list of human-only actions — "what do you need from me", "what's blocked on hardware/me", "what should I go do". Produces human actions, not agent tasks — for agent work use fetch-work instead.
---

# unblock

Produce a ranked list of actions only Jeremy can do — physical device access
or power-cycling, hands-on tests (e.g. double-tap trials), purchases
(adapters, SD cards, parts), account/infra decisions, or approving a risky
live-Pod deploy — that would unblock the most downstream agent work.

This is the complement of `fetch-work` (which ranks work an agent can pick
up); read `.claude/skills/fetch-work/sources.md` for the same source list —
don't re-derive it here.

## Procedure

1. Query every source in `sources.md` (Gitea issues, README remaining-work,
   REPLACEMENT_PLAN gaps, memory dir, TODO/FIXME grep).
2. From each, extract only items that are *actually* stuck on something an
   agent cannot do: needs eyes/hands on the physical Pod, needs a purchase,
   needs a decision only Jeremy can make (e.g. accepting a safety trade-off,
   choosing between two designs), or needs explicit go-ahead before a
   live-device action per the "confirm before anything that touches the live
   Pod" rule in root `CLAUDE.md`.
3. For each, note: the action, effort (minutes/hours), and what it unblocks
   (name the downstream agent work — an issue, a TODO, a plan gap).
4. Rank by unblocking value (how much downstream work opens up) first,
   effort second (cheap-and-high-value first).

## Do not pad

Most sessions, little or nothing is genuinely blocked on Jeremy — agents can
read code, write code, run tests, and push branches without him. If the query
turns up nothing that truly requires his hands/wallet/decision, say plainly
that nothing is blocked on him right now, rather than manufacturing filler
items ("maybe review this sometime") to make the list look substantial. A
one-line "nothing blocked" answer is a correct, complete answer.
