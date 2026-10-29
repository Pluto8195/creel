# The Work Outline

The outline arranges a project's chats under work items. An item can be a workstream, a ticket, a PR or an investigation. Every key, menu and rule is in the [reference](../work-outline.md). This page covers what you use every day.

## Items

- **New item:** `⇧⌘I` or `N` (a sibling), `⇧N` (a child). Type the title and press Enter.
- **Rename:** `F2` or `R`.
- **Move:** `⇧↑` / `⇧↓` reorder, `⇥` / `⇧⇥` indent and outdent, or drag and drop. Dropping a chat on an item puts the chat in it.
- **Status:** Pending, Active or Completed, from the row menu. An item with open Required Actions shows **User Action Required**, and the count rolls up to its parents.
- **Archive:** `⇧⌘A`. Nothing is deleted when you restructure.

## Instructions

An item can carry instructions. A chat started in it gets, in order, the Space's instructions, its parents' instructions, then its own. Turning off inheritance on an item starts a new boundary, but the Space's instructions still apply.

- Edit them in **Work Item Details** (`↩` on the row, or `⌘I`). They only save when you click **Save**.
- Type `/` in the editor to pick a skill.
- If you change the instructions after a chat has started, the chat shows **Instructions changed**. Click **Compare**, then **Apply** to use the new ones from the next turn. Codex asks to start a fresh session.
- Agents can propose instructions, but only you can accept them.

## Start Chat

Press `⌘↩` on an item (or click **Start Chat**). The dialog has the prompt, the agent, model, reasoning and approval pickers, and where the chat runs (the Space folder, an existing checkout, or a new worktree). Nothing starts until you click **Start Chat**.

## Required Actions

Agents ask for decisions on their item. Open **Required actions** in the outline toolbar to see all of them, oldest first.

- **Reply & resolve** sends your answer to the chat as a message and resolves the request.
- **Resolve** closes it without messaging the chat.

Mast notifies you when an item goes from no requests to some.

## Compact rows

An item with exactly one live chat shows as one row. The row splits into an item row and chat rows when you add a second chat, a child item, or a sub-chat. Press `⌘I` on any row to open its Details. On a compact row you can also click the status glyph.

## Undo

`⌘Z` undoes your last outline change and `⇧⌘Z` redoes it. Undo is kept in memory for this session and only covers changes you made in the UI, not ones an agent made. If the item changed since, Mast refuses and says nothing was changed. **Recent Changes…** in the row menu lists them.

## Try it

1. Make two items, "API" and "UI". Press `⇧↑` / `⇧↓` to reorder them, then `⇥` to nest "UI" under "API".
2. Add an instruction to "API", then start a chat in "UI". Open its **Effective instructions** in Details and see the inherited line.
3. Press `⌘Z` to undo the nesting.
4. Ask the chat to create a Required Action ("ask me which database to use") and answer it with **Reply & resolve**.
5. Press `⌘I` on a one-chat row.

<!-- TODO(Mikey): a screenshot of the outline with a nested item, a compact row and a User Action Required badge. -->
