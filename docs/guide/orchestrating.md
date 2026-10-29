# Orchestrating with coordinator and worker chats

One chat (the coordinator) can start other chats (workers) on items below its own and collect their results. You talk to the coordinator. The workers do the work in their own worktrees.

## How it works

Every Claude Code and Codex chat in a project gets the `mast-agent` MCP server. The coordinator uses these tools:

- `work_outline_create_child` makes a child item under its own item.
- `create_chat { workItemId, prompt, model, idempotency_key }` starts a worker on its item or any item below it. It uses the same launch path as **Start Chat**, so the target item's instructions and execution setup apply. A worker is never given a wider sandbox than its parent.
- `work_outline_read_subtree` lists the items, chats and open Required Actions below the coordinator.
- `wait_for_turn`, `read_chat` and `list_chats` read a worker's progress and reply.
- `send_message` sends a worker a follow-up.
- `work_outline_resolve_user_action_request` resolves a request a direct child raised.

Workers cannot message their coordinator. They finish by raising a Required Action, and the coordinator finds it with `work_outline_read_subtree`.

## The Ready-to-merge flow

1. The coordinator creates an item per task and starts a worker chat on each with a self-contained prompt.
2. Each worker makes its own branch and git worktree, calls `bind_chat_worktree` so the Changes pane shows that worktree, does the work, pushes the branch and marks its item completed.
3. The worker raises a Required Action titled **Ready to merge: <branch>**. The detail says what changed, which tests ran and what to try.
4. The coordinator sees the action, merges the branch, and resolves the action.
5. Meanwhile the status light and Dock badge show you which chats are working and which need you. A **Ready to merge** action counts as ready, not as needing you.

The merge step belongs to one chat. Say so in the instructions on the coordinator's item.

## Coordinator tips

- Don't block on one worker. A worker that runs tests in the background goes idle before it is finished, and one that pushes a checkpoint is not done either. The **Ready to merge** action is the real signal.
- Keep the coordinator thin. A long-lived coordinator with a huge context is the most expensive chat you have. Keep state in a doc in the repo, and restart the coordinator often.
- Put big or noisy work (screenshots, design rounds, long searches) in workers.
- Long Required Action details come back cut short. Use `read_chat` on the action's origin chat for the full text.

## Prompts

Put the standing rules in the instructions on the coordinator's item, so every chat started from it inherits them. For example:

```
You are the coordinator for this project. Talk to me; delegate work.
- Create or find an item for each task, then start a worker with create_chat
  { workItemId, prompt, model, idempotency_key, wait: false }.
- Never wait on one worker in the foreground. Read the subtree for open
  "Ready to merge" actions.
- Only you merge, release and edit the handoff doc.
- Workers work on a feature branch in a git worktree, never on main.
```

A worker prompt should stand on its own:

```
Goal: <one sentence>.
Files you own: <paths>. Don't edit anything else.
Done when: <tests pass / behaviour you can try>.
Work in a git worktree on branch <name>, and call bind_chat_worktree.
Commit early and push checkpoints. When finished, push, mark your item
completed and raise a Required Action "Ready to merge: <name>" saying
what changed, which tests you ran and what to try. Do not merge.
```

## Try it

1. Make an item "Coordinator" with the instructions above.
2. Start a chat on it and ask: "Create two child items, 'Add a README badge' and 'Fix a typo', and start a worker on each. Don't wait for them."
3. Watch both appear under the item in the outline.
4. When a worker raises **Ready to merge**, open the Required Actions list and read it.
5. Ask the coordinator to merge it and resolve the action.

<!-- TODO(Mikey): a recording of steps 2-4 (two workers appearing, the status light changing, the Ready to merge action) would make this page much clearer. -->
