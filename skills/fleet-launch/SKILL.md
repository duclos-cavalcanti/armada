---
description: Push the current branch then create a draft GitLab MR via /create-mr
argument-hint: [target_branch]
---

# Skill: fleet-launch

You are opening a Merge Request for the current branch. This skill is a thin
wrapper around `/create-mr` that adds the missing push step — `/create-mr`
assumes the branch is already on the remote, so this skill pushes first.

**Argument:** `target_branch` — forwarded to `/create-mr`; defaults to origin's
default branch (`git symbolic-ref --short refs/remotes/origin/HEAD`, minus `origin/`).
A stacked-MR child passes its parent's source branch here (the dispatching caller
knows it from the ticket's `is blocked by` edge); everything else takes the default.

---

## Step 1 — Push the branch

Push the current branch to the remote, setting upstream tracking if not already
set:

```bash
git push -u origin HEAD
```

If the push is refused because the remote has diverged (e.g. after a UI-rebase),
use `git push --force-with-lease` instead. Do not use plain `--force` — the
lease check refuses if a teammate's push intervened since your last fetch.

If the push fails for any other reason (auth, network, protected-branch policy),
surface the error and exit non-zero. Do not retry blindly.

---

## Step 2 — Invoke /create-mr

Invoke `/create-mr` with the target branch (the argument, or the default
branch). `/create-mr` drafts the MR description, opens the MR via
`glab mr create --draft`, and displays the MR link.

The MR is left in draft (per `/create-mr`'s default).

---

## Failure handling

If the push or the `/create-mr` invocation fails in a way you can't resolve,
write a summary (which step failed, the error, what was tried) to **both** the
Jira ticket *and* your claude-code response, then exit non-zero. Do not paper
over a push failure with `--force` to avoid losing teammates' work.
