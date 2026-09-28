---
name: template-feedback
description: Raise a reusable, project-agnostic improvement to the AI-engineering workflow itself back to the template. Use when you discover a better convention, a gap or clearer wording in AGENTS.md, a skill that could be sharper, or a docs-taxonomy fix that would help ANY project using this template — not for project-specific code, config, or content. It surfaces the improvement to the user with a concrete proposal, and on the user's OK opens a GitHub issue on the upstream template repo (never edits the template silently). May be user-invoked as `/template-feedback [idea]`, or reached for proactively the moment such an improvement surfaces during other work.
argument-hint: "[short description of the improvement]"
---

# Propose a template improvement upstream

This skill closes a feedback loop: when working on a project that adopted the **agentic-engineering-template**, you will occasionally discover a way the *workflow itself* could be better — a rule that should exist in `AGENTS.md`, a skill step that misfires, a docs folder whose purpose blurs, a principle worth stating. Those improvements are worthless if they die in one project's chat. This skill routes them back to the canonical template so **every** adopter benefits.

**Upstream target:** `dsaenztagarro/agentic-engineering-template` (the canonical template). If this repo is a fork of the template or you maintain your own canonical copy, confirm the correct upstream slug with the user before creating anything.

## When to use it — and when NOT to

Use it **only for generalizable, project-agnostic** improvements to the agentic workflow: things that would help an arbitrary project that adopts the template.

| Use the skill | Do NOT use the skill (handle locally instead) |
|---|---|
| A missing/weak rule in `AGENTS.md`'s portable sections | A project-specific rule → edit *this* repo's `AGENTS.md` |
| A skill (`epic`, this one, …) whose steps could be clearer/safer | A bug in this project's code or tests |
| A docs-taxonomy gap (what belongs in a record, a test, the README or a skill) | A one-off decision → the theme's record in *this* repo |
| A portable principle worth adding (testing, model selection, docs style) | Project content, copy, or config |

The tell for "generalizable": the improvement is phrased without naming this project, its language, or its framework. If you cannot state it project-agnostically, it belongs in this repo's `AGENTS.md`, not upstream — apply it there and stop.

## Operating principles

- **Never edit the template silently.** The default output is a *proposed* GitHub **issue** on the upstream repo, opened only after the user approves. Do not open a PR or push to the template unless the user explicitly asks for a PR.
- **Raise the hand promptly, but don't derail.** When an improvement surfaces mid-task, note it and finish the current unit of work first; then run this skill. Don't silently pocket the idea, and don't abandon the task to chase it.
- **One improvement per issue.** Keep proposals atomic so each can be triaged and accepted on its own. Batch nothing.
- **Propose, don't decide.** You surface the change and the reasoning; the human triages. Show the evaluation, not just the outcome.

## 1. Confirm it's generalizable

Restate the improvement in one project-agnostic sentence. If you can't (it names this project/language/framework), it's local: apply it to this repo's `AGENTS.md` or the theme's decision record and **stop** — do not take it upstream.

## 2. Locate the artifact it touches

Identify the single template file the change lands in, e.g.:

```
AGENTS.md                                  # a rule/principle
.claude/skills/<name>/SKILL.md             # a skill's steps
docs/adr/README.md                         # decision-record conventions
docs/adr/0001-decision-records.md          # what goes where, and why it is bounded
README.md                                  # the template's own overview
```

## 3. Dedupe against existing issues

Before proposing, check the upstream repo so you don't file a duplicate:

```bash
gh issue list --repo dsaenztagarro/agentic-engineering-template --search "<keywords>" --state all
```

If a matching issue exists, add a comment to it (with the user's OK) instead of opening a new one.

## 4. Draft the proposal and surface it

Present to the user, in-conversation, a compact proposal:

- **Problem** — what's missing or misfiring today, and where it bit you (one concrete instance).
- **Why it's better** — the improvement and the reasoning; note any trade-off.
- **Affected file** — from step 2.
- **Proposed change** — the concrete wording or diff, ready to drop in.

Then ask whether to open the issue upstream.

## 5. On the user's OK, open the issue

```bash
gh issue create --repo dsaenztagarro/agentic-engineering-template \
  --title "<concise improvement title>" \
  --body "$(cat <<'EOF'
## Problem
<what's missing / misfiring, with the concrete instance>

## Proposed change
<the wording or diff, ready to apply>

## Affected file
<path in the template>

## Why it generalizes
<the project-agnostic rationale>

_Raised via the template-feedback skill while working on a downstream project._
EOF
)"
```

Report the issue URL back. If the user instead asks for a **PR**, branch the upstream repo, apply the exact proposed change, run the template's own gate if any, and `gh pr create` — but that is opt-in, never the default.

## Guardrails

- No upstream write (issue or PR) without explicit user approval in the current turn.
- Never rewrite a decision record or a skill's contract as a "fix" — propose the change and let the human decide.
- If unsure whether an idea is generalizable or project-local, ask the user rather than guessing.
