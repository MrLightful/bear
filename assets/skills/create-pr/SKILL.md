---
name: create-pr
description: Write and publish a GitHub pull request when Bear queues its Create PR action.
metadata:
  source: https://github.com/mattpocock/skills/blob/main/skills/engineering/pr/SKILL.md
  credits:
    skill: show-me
    author: Dex Horthy
    organisation: Humanlayer
    url: https://github.com/humanlayer/skills/blob/main/plugins/show-me/skills/show-me/SKILL.md
---

## Inspect

Run the supplied Inspect command in the conversation’s checkout. Save its `head_oid` and `pending_tree`. Review the topic branch and pending changes against the returned `configured_base` or, when unset, the repository’s default branch. Stop if Git reports conflicts or an operation in progress.

## Write the PR

Give it a concise title. Start the description with the summary and use the repository’s domain terms from `GLOSSARY.md` when available.

```markdown
## Summary

<Small visual showing the change>

## Evidence

- **Before:** <Observed baseline>
- **After:** <Observed result>

## Merge Danger

**Door:** <One-way or two-way>

**Blast Radius:** <Scope in a word or short phrase>
```

- **Summary.** Choose the smallest visual that explains what changed. Use pseudocode for logic, a call tree for runtime flow, a component tree for UI structure, a shallow file tree for ownership, or Mermaid for interactions. Use a diff sketch when the surrounding structure helps readers locate the change. Add a short sentence when readers need the reason for the change.
- **Evidence.** Prefer before-and-after screenshots for visual changes and actual test or command output for behavior changes. Name the check and show the relevant result. If either side is unavailable, say what was verified and what remains unverified.
- **Merge Danger.** Use **two-way** when rollback is straightforward and **one-way** when the effects are hard to reverse. Name the affected area in **Blast Radius**. Add a sentence when reviewers need rollout, compatibility, or rollback context.

## Publish

Pass one JSON object on stdin to the supplied Publish command. Copy `head_oid` and `pending_tree` from Inspect. Include `title`, `body` as a JSON string containing the Markdown, and `commit_subject` as a single-line message for pending changes or `null` for a clean checkout. Use Bear’s publisher for the commit, push, and GitHub write.

Report the PR URL. If the publisher returns a compare URL, explain that the form still needs to be submitted and include the drafted body. If it fails, report the blocker and any writes already completed.
