# Work Outline

The Work Outline organizes a project's Chats under nested **work items**. A work
item can stand for a workstream, a ticket, a pull request, or an investigation.
It holds the instructions its Chats inherit, the folders and repositories they
run in, and the questions agents have asked you. Chats stay the main working
surface. The Outline is a third Overview view beside List and Canvas, and a Chat
never needs a work item.

| Concept | What it is |
| --- | --- |
| Work item | A title with an optional description, a status (Pending, Active, Completed, or the derived User Action Required), child items, associated Chats, and external references. |
| Instructions | Plain-text directives attached to the Space root or to a work item. A Chat receives the Space-root instructions, then its ancestors' instructions, then its own item's instructions, as runtime context outside the transcript. |
| Execution setup | The execution root, primary repository, member repositories, and optional pinned worktrees that a work item's Chats run in. Items inherit the setup of their nearest ancestor. |
| Required actions | Durable requests from agents (questions, approvals, decisions) that keep an item in User Action Required until they are resolved. |
| Agent tools | Work Outline tools that agents in a project Chat can call to read their item, create children, set status, ask you for something, or propose an instruction. |

## Quick start

1. Open the Overview and choose **Outline**, or press `⇧⌘L` (`Ctrl+Shift+L`).
2. Create a work item with **New item**, `⇧⌘I` (`Ctrl+Shift+I`), or `N`. Type a
   title and press Enter. In a project with no work items, the Outline offers
   **Create work item** in place of the empty tree.
3. Select the item and press Enter to open **Work Item Details**. Under
   **Instructions**, add an instruction and click **Save**.
4. Click **Start Chat** on the selected row or in Details, or press `⌘↩`. The Chat launches with
   the item's effective instructions and execution setup, and it is associated
   automatically.
5. To bring existing Chats under the item, drag their rows onto it or run
   **Associate Chat with Work Item**.

Press Enter on a Chat row to open the Chat beside the Outline with its composer
focused. Press `⌘;` (`Ctrl+;`) to jump back to the Outline tree with that Chat's
row selected, and press it again to return to the Chat. Press `⇧⌘↩`
(`Ctrl+Shift+Enter`) to open that Chat as the full chat. See
[Jumping between a Chat and the Outline](#jumping-between-a-chat-and-the-outline).

Press `?` while the Outline has focus, or click the keyboard button in the
Outline toolbar, to open the shortcut reference. Hover over any Outline control
to see what it does and its current key. Right-click any row for everything you
can do with it.

## Keys

Outline keys work only while the Outline tree has focus. They never fire in the
inline editor, a picker, the search field, or any other text input.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Previous / next row (display order) | `↑` / `↓` | `↑` / `↓` |
| First / last row | `Home` / `End` | `Home` / `End` |
| Collapse / expand | `←` / `→` | `←` / `→` |
| Previous / next sibling (skips subtrees) | `⌥↑` / `⌥↓` | `Alt+↑` / `Alt+↓` |
| Open Work Item Details | `↩` | `Enter` |
| Start a Chat in the selected work item (on a Chat row: in the Chat's work item) | `⌘↩` | `Ctrl+Enter` |
| Rename (a work item in place; a Chat row opens the rename dialog) | `F2` or `R` | `F2` or `R` |
| New sibling / new child | `N` / `⇧N` | `N` / `Shift+N` |
| Move before previous / after next sibling | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Indent (last child of the previous sibling) | `⇧→` or `⇥` | `Shift+→` or `Tab` |
| Outdent (right after the former parent) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Chat row: move to the previous / next work item | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Chat row: move to the parent item (out of a top-level item) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Shortcut reference | `?` | `?` |
| Archive the selected Chat, or open **Archive…** for the selected work item | `⇧⌘A` | `Ctrl+Shift+A` |
| Context menu of the selected row | `⇧F10` | `Shift+F10` or `Menu` |

`⇧⌘A` is the app's **Archive session** shortcut (rebindable in **Settings →
Shortcuts**). Outside the Outline tree it archives the open Chat as before. In the
tree it acts on the selected row: a Chat row is archived the way the sidebar
archives it and the selection moves to the next row, a work item opens its
Archive dialog with the choice for its descendants, and project and group rows
ignore it. The **Archive Chat** and **Archive…** menu entries show the key.

Every Work Outline command is also in the command palette and can be rebound in
**Settings → Shortcuts**. Only four commands ship with a global key, because
global keys also fire while a text field has focus:

| Command | Default |
| --- | --- |
| Open Outline | `⇧⌘L` / `Ctrl+Shift+L` |
| Toggle Outline Focus | `⌘;` / `Ctrl+;` |
| Open Full Chat | `⇧⌘↩` / `Ctrl+Shift+Enter` |
| Create Work Item | `⇧⌘I` / `Ctrl+Shift+I` |
| Create Child Work Item, Rename, Move, Associate Chat with Work Item, Open Effective Instructions, Create Instruction from Chat, Open Required Actions, Start Chat in Work Item, Move Up / Down, Indent / Outdent, Show Outline Shortcuts | Unbound |

**Start Chat in Work Item** has no global key because `⌘↩` (`Ctrl+Enter`) sends
from the composer, but in the Outline tree `⌘↩` starts a Chat in the selected
work item. On a Chat row it starts one in that Chat's work item, and shows a
notice when the Chat is not in one.

Commands that need a selected work item open the Outline and show a short notice
when nothing is selected. Tooltips and the shortcut reference always show the
current bindings, including keys you have rebound.

### Jumping between a Chat and the Outline

**Toggle Outline Focus** (`⌘;`, `Ctrl+;` on Windows and Linux) moves the
keyboard between a Chat and the Outline tree. It works while you type in the
composer. Earlier builds used `⌘\`, which 1Password takes system-wide for
autofill. If you never changed that key, it moves to `⌘;` automatically; a key
you chose yourself is kept.

- From a Chat (the full chat or the Chat beside the Outline), it switches to the
  Outline, selects that Chat's row, expands any collapsed parent items, scrolls
  the row into view, and focuses the tree. If the Outline does not show the
  Chat (a Chat without a project, or one hidden by a filter), the selection
  stays where it was and the tree is focused.
- From the Outline tree, it returns to the last Chat you opened from the Outline
  (or the Chat you jumped from), in the same layout, with its composer focused.
  If there is no such Chat, the Outline shows a short notice.

The jump never changes which Chat is selected and never clears a composer
draft. `⇧⌘L` (`Ctrl+Shift+L`) does the same Chat-to-row jump when you press it
in the composer of a Chat you opened from the Outline. `Esc` in the Outline is
not part of this: it still closes Details and menus.

### Opening the full chat

**Open Full Chat** (`⇧⌘↩`, `Ctrl+Shift+Enter` on Windows and Linux) does what
the chat panel's **Open full chat →** button does: it closes the panel and
switches to the full chat with its composer focused. It works while you type in
the panel's composer, and a draft you started there is still there. It opens:

- the Chat on the selected Outline row, while the Outline tree has focus;
- otherwise the Chat beside the Overview (the highlighted row in List or
  Canvas);
- otherwise the Chat on the selected Outline row.

With none of these, the Outline shows a short notice. In the full chat, Toggle
Outline Focus takes you back to that Chat's Outline row. The command is also in
the command palette and on every Chat row's menu, and **Open full chat →** shows
its key when you hover over it.

## Context menus

Right-click a row (or Control-click it on macOS) to select it and open its
menu. With the Outline focused, `⇧F10` (`Shift+F10`, or the `Menu` key on
Windows and Linux) opens the menu of the selected row. Each entry shows its
current key, including keys you have rebound; an entry without a key shows
none. Entries that cannot apply are greyed out, and hovering over one tells you
why (for example, **Move Up** on an item that is already first, or **Outdent**
at the top level).

| Row | Entries |
| --- | --- |
| Work item | Open Details, Start Chat, Rename, New Sibling, New Child, Move Up, Move Down, Indent, Outdent, Move To…, Set Status (Pending, Active, Completed), Associate Chat…, Open Effective Instructions, Add Instruction, Copy Path, Archive…, Restore |
| Chat | Open Chat, Open Full Chat, Rename…, Associate with Work Item… (or Move to Work Item…), Move to Previous Item, Move to Next Item, Move to Parent Item (Move to Unassociated Chats from a top-level item), Remove from Work Item, Compare Instructions… and Apply Current Instructions (when the instructions changed), and the sidebar's Pin, Copy name + IDs, Close out worktree…, and Archive Chat |
| Space root (a project row, or **Space root instructions** in a single-project Outline) | New Work Item, Space Instructions, Execution Setup, Required Actions, Show Outline Shortcuts |
| Unassociated Chats | Associate “Chat”… for the selected Chat (no menu without one) |

In the menu, `↑` / `↓` move between entries, `↩` (`Enter`) runs one, `→` opens
**Set Status** and `←` returns, and `Esc` or a click outside closes it. Every
entry runs the same action as its key or command palette entry, so the same
confirmations apply. Set Status, Add Instruction, Archive…, Restore, Remove from
Work Item, Space Instructions, and Execution Setup open Work Item Details and
act there.

## Restructuring

Use the keys above, the matching palette commands, the **Move** picker (choose
**Place at Top** or **Bottom** with `←` / `→`), or drag and drop. Dropping on the
middle of a row makes the item the last child of that row. Dropping on the top
or bottom quarter places it before or after that row. Dropping a Chat row on a
work item associates the Chat with it. Descendants move with their item.
Restructuring never deletes anything.

Chats have no order inside an item, so with a Chat row selected the same keys
move the Chat between work items instead:

- `⇧↑` / `⇧↓` move it to the previous / next work item in row order, counted
  from the Chat's current item. Items at any depth count if the Outline shows
  them; Chat rows and archived items are skipped. From **Unassociated Chats**,
  `⇧↑` moves it to the last work item. Nothing happens at either end.
- `⇧←` (or `⇧⇥`) moves it to its item's parent. From a top-level item, it
  moves the Chat out of its work item to **Unassociated Chats**. Nothing
  happens for an unassociated Chat.
- `⇧→` and `⇥` do nothing on a Chat row.

Moves stay inside the Chat's project. Only the selected Chat moves: Chats
started from it keep their own work items, and a Chat shown under the Chat
that started it stays there while its work item changes. The selection follows the Chat,
and the same confirmation as **Move to Work Item…** appears when the Chat's
instructions would change. Rapid presses apply in order. Chats without a
project, and Chats that no longer exist, cannot move. A move never changes a
Chat's Canvas position, folder, or execution setup. The palette's Move Up /
Down and Indent / Outdent commands act on work items only. Archiving an item that has children asks
whether to archive them as well or move them to another parent first.

## Work Item Details

Details open in the Overview's right panel and restore the previous panel when
closed. Sections stay collapsed until you open them.

| Section | Contents |
| --- | --- |
| Header | Breadcrumb, title, status, **Start Chat**, **Archive…** or **Restore**. |
| Description | Free text. |
| Instructions | This scope's own instructions, the inheritance toggle (non-root items), reordering, and **Save** / **Cancel**. |
| Effective instructions | What a Chat launched here receives, in order, marked Local or Inherited with the source item, plus the precedence rule and the size budget. |
| Execution | The resolved setup and its source (this item, an ancestor, the Space default, or the Space folder), **Edit**, **Validate**, and **Reset to inherited**. Chats running with a different setup are listed as exceptions. |
| External references | PR, ticket, and URL links. They open according to your web-link preference. |
| Chats | Associated Chats. Use **Associate…** to add one or **Disassociate** to remove one. |
| Required actions | Open and resolved requests for this item and its descendants. |
| Proposed instructions | Instructions that agents have proposed and that are waiting for review. |

The Space root has its own Details (**Space root instructions** at the top of a
single-project Outline, or **Instructions** on each project row) for Space-wide
instructions.

## Instructions, revisions, and drift

- Instructions save only when you click **Save** (or press Enter in the editor).
  Closing Details, switching items, or pressing Escape with an unsaved draft asks
  you to keep editing or discard. Blur never saves.
- Turning off inheritance on an item starts a new boundary. Ancestor work-item
  instructions stop there, but Space-root instructions still apply.
- A Chat launch records an immutable **revision** of its effective instructions.
  A revision allows at most 9,000 characters and 32 entries.
- If the instructions change after launch, the Chat shows **Instructions
  changed** in its header, on its Outline row, and on List and Canvas cards. The
  Chat continues to use its applied revision until you act. Click **Compare** to
  see the difference, then **Apply** to use the current instructions.
- Apply never changes a running turn. It takes effect at the next turn:
  - **Claude Code** resumes the same conversation with the new instructions.
  - **Codex** cannot replace instructions inside a thread reliably. Your next
    send asks you to start a fresh session, and the agent loses the
    conversation's context if you confirm.
  - Cursor and ACP harnesses do not support Work Outline instructions. Sends
    that require instructions are rejected with an explanation.
- Before you associate or move a Chat, move an item, toggle inheritance, or edit
  or archive an instruction, a confirmation counts the affected Chats and items
  if the change would alter any associated Chat's instructions.
- **Create Instruction from Chat** turns the selected transcript text (or the
  last assistant message) into an instruction for this item, its parent, or the
  Space root. The dialog shows how many descendants it reaches.
- Type `/` in an instruction editor (item or Space root instructions, Create
  Instruction from Chat, or a proposal review) or in the Start Chat prompt to
  pick a skill, as in the chat composer. The list shows your user skills plus
  the project skills of the folder the item runs in (its resolved execution
  root, or the Space folder for Space-root instructions). Use the arrow keys to
  move, Enter or Tab to insert, and Escape to close the list. Instructions
  receive plain text such as `` `code-review` skill ``. Start Chat inserts
  `/code-review`, the same form the composer uses.
- Multi-line editors (instructions, description, proposal review, Create
  Instruction from Chat, and the Start Chat prompt) grow with their text from
  three lines up to 40% of the window height, then scroll inside the editor
  and keep the caret in view.

## Execution setup and validation

- The setup applies to the whole configuration: the nearest item that has its
  own setup wins entirely, and fields are not merged across levels. The
  execution root can be any folder except your home directory. Every configured
  repository must be a Git checkout.
- **Validate** checks the saved setup on the device that owns the project.
  **Start Chat** validates again, and Start stays disabled until validation
  passes. When the setup does not pin a worktree, the dialog lets you choose the
  Space workspace, an existing checkout, or a new worktree.
- Zeron does not fall back to another checkout. If a path, repository, or
  worktree is missing, moved, pruned, or offline, launch or resume is blocked
  with the problem and how to fix it. Zeron never creates or switches a branch
  silently.
- Each Chat keeps the setup it launched with. Later edits never retarget existing
  Chats, so a Chat whose setup now differs is marked as an exception (folder
  mark).

## Required actions and notifications

- When an agent needs something from you, it files a request on its item. The
  item shows **User Action Required** with a count, and status totals roll up to
  its ancestors.
- **Required actions** in the Outline toolbar lists every open request, oldest
  first, with its item, its originating Chat, and its age. Click **Open** to go to
  the Chat, or **Resolve** (optionally with a note) to close the request.
  Resolved requests are kept under **Resolved**.
- Zeron notifies you when an item goes from zero to one or more open requests:
  one notification per item, with retries deduplicated. Notifications follow your
  notification, background-only, and input-sound settings. Requests always
  appear in Required Actions, even when notifications are off.
- Completing an item with open requests asks for confirmation and resolves them.

## Agent tools

Claude Code and Codex Chats that belong to a project get a `zeron-agent` MCP
server for the duration of each run. These are the Work Outline tools:

| Tool | Does |
| --- | --- |
| `work_outline_read_current` | Read the Chat's item and effective instructions. |
| `work_outline_create_child` | Create a child item (Pending or Active). |
| `work_outline_associate_current_chat` | Associate the calling Chat with an item. |
| `work_outline_set_status` | Mark an item Active or Completed. |
| `work_outline_create_user_action_request` | Ask you for something on the Chat's item. |
| `work_outline_resolve_user_action_request` | Resolve a request this Chat raised. |
| `work_outline_propose_instruction` | Propose an instruction for the item, its parent, or the Space root. |

Agents cannot:

- Add or change instructions. Proposals wait for your review.
- Complete an item that has open requests.
- Resolve another Chat's requests.
- Associate a Chat other than their own.
- Move their own Chat outside its current item's subtree, or re-associate a
  Chat that is bound to an execution setup.
- Act outside their own project and device.
- Widen their sandbox or enable auto-approve.

Every change an agent makes needs an idempotency key, so retries do not create
duplicate items.

## Limitations

- Work Outline instructions are added to Claude Code's system prompt. Zeron
  cannot mechanically stop them from outranking repository instructions, so the
  instruction envelope states that they never override repository, global,
  skill, or safety instructions. Instructions do not reach harness-native
  subagents.
- Instructions have no semantic conflict detection. Within the Outline, the more
  specific instruction that comes later takes precedence.
- Codex revision changes require a fresh session (see above). Cursor and ACP
  harnesses cannot receive Work Outline instructions.
- List rows and Canvas cards show the work-item path, attention, drift, and
  exception marks, but not status totals or proposal counts. The sidebar shows
  no Outline indicators.
- Opening a request does not scroll to the exact message in the transcript.
  Resolved history shows the 50 most recent requests.
- Outline data is not copied when you import a local profile into a synced
  profile. Deleted rows are garbage-collected after 30 days.
- The engine's loopback IPC is unauthenticated for its general methods. Only
  agent-attributed Work Outline writes require the per-run token.
