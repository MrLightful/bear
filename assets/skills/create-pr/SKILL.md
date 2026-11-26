---
name: create-pr
description: Write and publish a GitHub pull request when Bear queues its Create PR action.
---

1. Run the supplied Inspect command in the requested checkout. Keep its `head_oid` and `pending_tree` for publishing. Compare the topic branch and pending changes with the intended PR base. Use the returned `configured_base` if present or the target repository’s default branch. Stop if you find conflicts or an unfinished Git operation.
2. Write a concise title and a Markdown body with a **Summary** of what changed and why, and **Verification** with checks that actually ran and their results. Include rollout or compatibility implications when relevant. Use the conversation and diff as evidence and identify checks still pending.
3. Run the supplied publisher command from the request. Send one JSON object on stdin with `head_oid` and `pending_tree` copied from Inspect, `title`, `body`, and `commit_subject` (a single-line message for pending changes, or null for a clean checkout). Encode the body as a JSON string. The publisher validates the target, commits pending changes, pushes the selected branch and opens the PR. Use it for all writes.
4. Report the URL on success. If the publisher returns a compare URL, say GitHub still needs the PR to be opened and include the drafted description. If it reports an error, explain the blocker and any completed writes it reports.
