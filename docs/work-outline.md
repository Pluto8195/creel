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

Moving the selection onto a Chat row, with the arrow keys or a click, shows
that Chat beside the Outline after a brief pause while the tree keeps focus, so
you can keep browsing with `↑` / `↓`. Moving onto a work item row leaves the
last Chat showing. While Work Item Details or Required Actions is open beside
the Outline, browsing leaves it open and shows no Chat. Showing a Chat this way
marks it seen the same way opening it beside the Outline does, and it never
opens the full chat.

Press Enter on a Chat row to open the Chat beside the Outline with its composer
focused. Press `⌘;` (`Ctrl+;`) to jump back to the Outline tree with that Chat's
row selected, and press it again to return to the Chat. Press `⇧⌘↩`
(`Ctrl+Shift+Enter`) to open that Chat as the full chat. See
[Jumping between a Chat and the Outline](#jumping-between-a-chat-and-the-outline).

Press `?` while the Outline has focus, or click the keyboard button in the
Outline toolbar, to open the shortcut reference (its **Linear** group covers the
Linear panel's issue rows and project-linked items). Hover over any Outline control
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
| Start a Chat in the selected work item (on a Chat row: in the Chat's work item; on a PR review item: review its PR, see [PR review sync](#pr-review-sync)) | `⌘↩` | `Ctrl+Enter` |
| Rename (a work item in place; a Chat row opens the rename dialog) | `F2` or `R` | `F2` or `R` |
| New sibling / new child | `N` / `⇧N` | `N` / `Shift+N` |
| Move before previous / after next sibling | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Indent (last child of the previous sibling) | `⇧→` or `⇥` | `Shift+→` or `Tab` |
| Outdent (right after the former parent) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Chat row: move to the previous / next work item | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Chat row: move to the parent item (out of a top-level item) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Project row (more than one project): move the project up / down | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Shortcut reference | `?` | `?` |
| Archive the selected Chat, or open **Archive…** for the selected work item | `⇧⌘A` | `Ctrl+Shift+A` |
| Context menu of the selected row | `⇧F10` | `Shift+F10` or `Menu` |

`⇧⌘A` is the app's **Archive session** shortcut (rebindable in **Settings →
Shortcuts**). Outside the Outline tree it archives the open Chat as before. In the
tree it acts on the selected row: a Chat row is archived the way the sidebar
archives it and the selection moves to the next row, a work item opens its
Archive dialog with the choice for its descendants, and project and group rows
ignore it. The **Archive Chat** and **Archive…** menu entries show the key.

### Search

`⌘F` (`Ctrl+F`) focuses the Overview's search field and selects its text, in
List, Canvas, and Outline. It is one shared field: in the Outline it filters the
rows ("Nothing matches the search" when none do). The key is fixed (not in
**Settings → Shortcuts**) and works with or without focus in the Overview; the
file editor's own Find still wins inside a file, and in a full chat `⌘F` does
nothing. In the search field:

| Key | Action |
| --- | --- |
| `Esc` | Clear the query; when it is already empty, return focus to the List, Canvas, or Outline tree (with a side panel open, an empty-field `Esc` closes the panel first) |
| `↩` / `↓` | Select the first matching row in List or Outline (Enter on Canvas moves focus to the canvas) and focus it, so the arrow keys work at once |

In List, `↑` / `↓` then move the selected row (`↑` on the first row returns to the
search) and `↩` opens the selected Chat beside the Overview.

Every Work Outline command is also in the command palette and can be rebound in
**Settings → Shortcuts**. Only five commands ship with a global key, because
global keys also fire while a text field has focus:

| Command | Default |
| --- | --- |
| Open Outline | `⇧⌘L` / `Ctrl+Shift+L` |
| Toggle Outline Focus | `⌘;` / `Ctrl+;` |
| Open Full Chat | `⇧⌘↩` / `Ctrl+Shift+Enter` |
| Create Work Item | `⇧⌘I` / `Ctrl+Shift+I` |
| Toggle Linear Panel (only while a project has Linear on) | `⌥⌘L` / `Ctrl+Shift+E` |
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
| Space root (a project row, or **Space root instructions** in a single-project Outline) | New Work Item, Space Instructions, Execution Setup, Required Actions, Show Outline Shortcuts. A project row also has Move Up, Move Down, Hide from Outline (Unhide on a hidden project), Rename Project…, and Delete Project… |
| Unassociated Chats | Associate “Chat”… for the selected Chat (no menu without one) |

Linear and PR review sync add their own entries where they apply (see
[Linear](#linear) and [PR review sync](#pr-review-sync)).

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
Restructuring never deletes anything. A move that changes assigned Chats'
instructions shows an **Undo** toast instead of asking first (see Instructions,
revisions, and drift).

Thin guide lines show what is nested under what. Each parent's line runs down
from its disclosure triangle and turns into each child row (├, or └ for the
last child), and it stops after the parent's last child. Chat rows get the same
lines, slightly dimmer. The selected row's path up to its top-level item, and
the lines of its own children, are drawn in the accent color. Hover over a line
to highlight everything under that parent.

While you drag a work item, a Chat, or a Linear issue, the Outline shows where
it will land. A before/after line starts at the new parent's guide, so its
depth tells you the parent. A target row for "into" drops is highlighted. A
small label names the spot, for example **Into API** or **Before UI (under
Launch v2)**. After a keyboard move (`⇧↑` / `⇧↓`, `⇥` / `⇧⇥`), the moved row and
its new parent's line flash briefly. The flash is skipped when the system's
Reduce Motion setting is on.

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
and the move applies at once, with the same **Undo** toast as **Move to Work
Item…** when the Chat's instructions change. Rapid presses apply in order. Chats without a
project, and Chats that no longer exist, cannot move. A move never changes a
Chat's Canvas position, folder, or execution setup. The palette's Move Up /
Down, Indent, and Outdent commands (and any shortcut you bind to them) do the
same as the keys on a Chat row. Archiving an item that has children asks
whether to archive them as well or move them to another parent first.

## Projects in the Outline

With more than one project, the Outline shows one collapsible row per project.
You can arrange and declutter these rows. Order and hidden projects are saved
on this device only, apply right away, and never change the project itself.
A single-project Outline has no project rows, so none of this applies there.

- **Reorder.** Select a project row and press `⇧↑` / `⇧↓`, choose **Move Up** /
  **Move Down** in its menu, or drag it onto another project row. Dropping on
  the upper half of a row places the project before it, and the lower half
  places it after. Projects you have never moved, including new ones, follow
  the ones you arranged, in alphabetical order.
- **Hide.** Choose **Hide from Outline** in a project row's menu. The project
  and its Chats disappear from the Outline only. The sidebar, List, and Canvas
  still show them. While any project is hidden, the Outline toolbar shows
  **Hidden projects (N)**. Click it to list hidden projects as dimmed rows, and
  click **Unhide** on a row (or choose **Unhide** in its menu) to bring it back.
- **Rename Project…** opens the same rename dialog as the sidebar's project menu.
- **Delete Project…** opens the sidebar's **Remove project?** confirmation.
  Removing a project permanently deletes its sessions: its Chats leave Zeron's
  index, and its whole Work Outline (work items, instructions, and requests) is
  deleted. This can't be undone. To just declutter the Outline, use **Hide from
  Outline** instead. Cancel deletes nothing.

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
- Moving a work item (keys, drag and drop, menu, **Move To…**) or moving,
  associating, or disassociating a Chat (including **Associate…** and
  **Disassociate** in Work Item Details) applies at once. If it changes an associated Chat's
  instructions, a toast says "Moved <title> · instructions changed for N chats
  (they keep their current instructions)" with **Undo**. The toast does not take
  focus, so keys keep working in the tree. It closes itself after about six
  seconds, or on Esc or ✕. Rapid moves share one toast whose count grows, and
  **Undo** reverts only the most recent move.
- Before you toggle inheritance, edit or archive an instruction, or change
  execution configuration, a confirmation counts the affected Chats and items if the change would alter
  any associated Chat's instructions.
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
- The Start Chat dialog picks the agent with the chat composer's own model
  picker: harness, model, reasoning effort, and model options, plus the Ask /
  Auto approval chip. It starts with what a new chat in the composer would use
  (your last-used picks), not the selected chat's settings. The new Chat runs
  with that selection, and the selection becomes your last-used pick for new
  chats. Tab moves from the prompt to the model chip and then the approval chip.
  Enter or Space opens the picker or toggles approval. Escape closes an open
  picker first, then the dialog. Enter in the prompt starts the Chat.
- Review PR starts its Chat with the same new-chat selection.
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
  the Chat. The same list appears in Work Item Details (the item and its
  descendants) and in the Space root's details (the whole Space).
- Requests that [PR review sync](#pr-review-sync) raises have no Chat: their
  origin reads **GitHub**, **Open** shows the PR item (or the PR once the item
  is gone), **View PR** opens the PR, and **Resolve** is the only answer
  (there is no agent to reply to). They resolve themselves when you submit
  the review or the PR is merged, closed, or no longer asks for you.
- **Reply & resolve** answers the agent. Type your answer in the note box and
  press Enter (or Cmd+Enter). Zeron sends it to the Chat that asked, as your
  own message: `Re: "<request>": <your note>`, with the request cut to about
  120 characters. Zeron resolves the request with the same note only after the
  Chat accepts the message:
  - If the agent is idle, the message starts its next turn.
  - If the agent is busy, the message is queued until the turn ends, like a
    message you queue from the composer.
  - The message uses the Chat's selected instruction revision.
  - If the Chat refuses the message, the request stays open and the error
    shows. Reasons include a host that can't deliver the instructions, a
    context reset waiting for your confirmation in the Chat, or an execution
    setup that no longer validates. **Open chat** next to the error takes you
    there.

  Reply & resolve stays disabled until you type a note. Retrying never sends
  a second message. If the message went out but the resolve failed, the retry
  only resolves. After a reply, **Open chat** appears on the resolved
  request.
- **Resolve** closes a request without messaging the Chat. The note is
  optional and is only recorded on the request. If the Chat that asked was
  archived or deleted, Resolve is the only choice: Reply & resolve is
  disabled, and its tooltip says why. Escape or **Cancel** closes the note
  box. Resolved requests are kept under **Resolved**.
- Zeron notifies you when an item goes from zero to one or more open requests:
  one notification per item, with retries deduplicated. Notifications follow your
  notification, background-only, and input-sound settings. Requests always
  appear in Required Actions, even when notifications are off.
- Completing an item with open requests asks for confirmation and resolves them.

## Linear

Linear is off until you turn it on for a project, and it never writes to
Linear. While it is off, a project shows no Linear UI, makes no Linear calls,
and keeps pasted Linear URLs as plain links. Nothing is added to the Outline
automatically: items and links come only from the actions below.

- **Connect.** In **Settings → Linear**, paste a Linear personal API key and
  click **Save**. Zeron checks the key with Linear before storing it in this
  Mac's Keychain, then shows **Connected as \<name\>**. The key is never shown
  again or given to agents. **Disconnect** removes the stored key. If Linear
  rejects the key later, Zeron deletes it and asks you to reconnect.
- **Turn it on.** Open the project's Space-root details (**Instructions** on the
  Space row) and switch on **Linear**. Without a key, **Connect Linear…** opens
  Settings → Linear. The first time you paste a Linear URL in a project with
  Linear off, Zeron offers **Enable** once.
- **Linear panel.** Press `⌥⌘L` (`Ctrl+Shift+E` on Windows and Linux;
  rebind **Toggle Inbox column** in Settings → Shortcuts), click **Inbox** in
  the Outline toolbar, run **Toggle Inbox Column** from the command palette, or
  choose **Open Linear Panel** on a Space row. The panel opens in the
  **Inbox** column on the left of the Outline (List and Canvas too), above
  the [GitHub section](#pr-review-sync) when that applies, so the chat panel,
  Work Item Details, and Required Actions stay open beside it on the right.
  The column's × closes both.
  - The key works anywhere in the Overview, including the chat panel's
    composer. With the panel closed it opens it and puts the cursor in its
    search. With the panel open and the cursor elsewhere, it moves the cursor
    to the search. With the cursor in the panel, it closes the panel and
    returns to the Outline tree. `Esc` in the panel also closes it.
  - With no project on Linear and none with PR review sync, the key and the
    command only show a notice.
  - Drag the panel's right edge to resize it (double-click the edge to reset).
    Its width and whether it is open are remembered on this device. If the
    window is too narrow for the panel, the Outline, and the right-side panel,
    the Linear panel narrows first, then floats over the Outline's left edge
    instead of squeezing it.
  - With Reduce Motion on, the panel opens and closes without animating.

  The panel lists **My issues** and **My projects**, each header with its count
  (of rows matching the search). Click a header to fold or unfold its list, or
  focus it and press `←` to fold and `→` to unfold. Expand a project to see its
  open issues. Folded sections and expanded projects are remembered on this
  device. Search filters both lists. Lists load when the panel opens and when
  you click ↻. Rows that an item in this project already links show
  **Linked · Reveal**, which selects that item in the Outline. With several
  Linear projects on, click the project name in the panel header to switch.
- **Create work item** (**Create** on an issue, or double-click the issue, or
  focus its row and press `↩`) adds `ENG-123 · title` as the last child of:
  - the work item linked to the issue's Linear project, whatever is selected
    in the Outline. If several items link that project, the most recently
    linked one wins. The button then reads **Create under \<item\>**.
  - otherwise the selected work item. With a Space row selected it goes to the
    top level. With nothing usable selected, the panel asks for the parent.

  Dragging an issue onto an Outline item always creates it there. The new item
  is selected when it appears.
- **Link** on an issue or project links it to the selected work item.
- **Create nodes for selected** (check issues in **My issues** or in an
  expanded project) creates one item per checked issue, in order, each by the
  same rule: under its project's linked item, else under the selected item.
  When they all share one linked item the button reads **Create under
  \<item\>**.
- **Add issues from \<project\>…** on a project-linked item (its right-click
  menu, or its Details **Linear** section) opens a picker of the project's open
  issues. It works with the Linear panel closed. Issues that already have a
  work item in this project show checked and greyed out, with **Added ·
  Reveal**. Search filters the list, **Select all** checks every shown issue
  not added yet, and **Create** adds one work item per checked issue as the
  item's last children, then selects the first new one. `Esc` or **Cancel**
  closes it.
- A project link made by pasting a project URL is keyed by the URL. It still
  counts for the rules above (Zeron matches the id at the end of the URL).
  Once the panel has loaded **My projects**, its project chip shows **Fix
  link**, which relinks it by the project's id (link anew, then remove the old
  link).
- Retrying a create or link never duplicates it.
- **Linked items.** Items linked to an issue show an icon cluster like a PR
  row's: **assignee · needs you · status** (the state dot and a status icon:
  done, canceled, started, backlog). The "N active · N user action required"
  text, the chat count and the references icon move into the hover, which also
  has the identifier, state, priority, assignee and title; click the status to
  open the issue. On a narrow Outline the state dot drops first, then the
  assignee; the attention dot and the status icon stay. Items linked to a
  project show a project chip. In Work Item Details, the **Linear** section has
  the full chip, ↻ (refresh every linked item in the project from Linear) and
  **Unlink**.
- **Synced fields.** An issue item's title and description follow Linear
  (Zeron syncs about every 15 minutes, and on ↻) until you edit them in Zeron.
  A field that still follows Linear shows **Synced from Linear**. When you link
  an issue to an item that already has a description, your description is kept
  and only the title follows Linear.
- **Pasting** a Linear issue or project URL into **External references** creates
  an issue or project link when Linear is on.
- **Right-click** an issue item for **Open in Linear**, **Refresh from Linear**,
  and **Unlink Linear Issue**, and a project-linked item for **Add Issues from
  \<project\>…**.
- If the engine on this device has no Linear support, Linear surfaces say
  **Linear isn't available on this host yet**.

## PR review sync

PR review sync keeps one work item per pull request that asks for your
review, in the project's GitHub repositories (the project folder's checkout,
its submodules, and nested repositories, matched by `origin`). It is off until
you pick a destination, and it never writes to GitHub. It runs on the device
that owns the project, through that device's `gh` login.

- **Pick the destination.** The quickest way is the Inbox column's GitHub
  section: while the sync is off it leads with **Auto-sync review requests
  into… Choose item ▾**, a searchable menu of the project's live work items
  plus **Create "PR reviews" item** (a new top-level item that becomes the
  destination). Once set, the section reads **Syncing into <item> · last
  synced 5m ago** with **Sync now**, **Change ▾**, and **Stop** (synced items
  stay where they are); click the item's name to reveal it. The Space-root
  details show the same: **Auto-sync review requests into… Choose item…**,
  then **Syncing into <item>** with **Change…** and **Stop**. Or right-click
  any work item and choose **Auto-sync PR Review Requests Here** (**Stop
  Syncing PR Reviews Here** on the destination). The destination row shows a
  small PR marker in the Outline ("PR review requests sync here").
- **What it keeps.** Each requested PR gets a child of the destination titled
  `<repo>#<number> · <PR title>` (renaming the PR later does not rename the
  item), linked to the PR, status Active, and a Required Action **Review
  requested: …** (**Re-review requested: …** for a later round, see
  [Required actions](#required-actions-and-notifications)). The sync runs
  every 3 minutes (first about a minute after the app starts), right after you
  set a destination, and on **Sync now** (GitHub section) or **↻ Sync now**
  (Space-root details, which shows the last result: `Synced: 1 created · 2
  updated · 0 resolved`, `Up to date`, or what went wrong). When you review, or the PR is merged, closed, or
  no longer asks for you, the round's request resolves and the item becomes
  Completed. A status you set yourself stands until GitHub's state changes
  again. Moved items keep syncing; archived ones are left alone.
- **PR icons.** Every item linked to a GitHub PR (synced or not) shows a row of
  fixed-width icons on the right, in projects with the sync, so they line up
  as columns: **author · needs you · your review · CI · PR**.
  - *Author:* the PR author's GitHub login, shortened to 10 characters (full
    login on hover), or **you** when you wrote it. Zeron learns your login
    once from `gh` (`gh api user`); until it has it, every author shows the
    plain login. The chat's work-context strip says "you" the same way.
  - *Needs you:* the usual attention dot (and count). It replaces the "N active
    · N user action required" text on these rows; the chat count moves to the
    hover.
  - *Your review:* eye = review requested, refresh = re-review requested
    (both in the attention color), green check = you approved, orange circle-x
    = you requested changes; empty when there is nothing for you.
  - *CI:* a dot (green passing, red failing, amber running; none without
    checks).
  - *PR:* the pull-request icon, colored by state (green open, grey draft,
    purple merged, red closed). Click it to open the PR (in Zeron or your
    browser, per **Open web links in Zeron**).
  Each icon has its own hover; hovering the group shows the whole line (`#42 ·
  Open · CI failing · Re-review requested · by alex · 2 chats`). On a narrow
  Outline the title keeps at least about 120 px and the icons drop in this
  order: CI, review, author; the attention dot and the PR icon always stay.
  The **?** reference has an icon legend, lists the plan keys (⌥↑ / ⌥↓ section
  jumps, ⌥⌘S for the section rail and its ↑ ↓ Home End, ⌘F / ⌘G / ⇧⌘G find)
  and ⌘P, **Open the PR page**. In Work Item Details, the **Pull
  request** section shows the full text chip and title with **↻ Refresh**,
  **Review**, **Fix CI**, and **Open on GitHub**; it refreshes when Details
  open. A synced item also shows **Synced from GitHub · round N**.
- **PRs on your items.** Items also get their own PRs linked automatically:
  an item linked to a Linear issue gets the GitHub PRs Linear attached to the
  issue (on the Linear sync), and any item gets a PR its Chat created
  (`gh pr create` in that Chat, or a PR you linked to the Chat yourself).
  Only PRs of the project's repositories; links are added, never removed or
  moved (a Chat moved to another item leaves its PR where it was), and a link
  you remove stays removed. The item's status is not changed when a PR
  merges. An item with several PRs shows one PR icon for each (up to three, then
  **+2** and so on, with the rest in its hover); the cluster's other slots
  describe the first PR. Details:
  [PRs on work items](pr-review.md#prs-on-work-items).
- **Review.** `⌘↩` (`Ctrl+Enter`) on a PR review item (a synced item, a child
  of the destination, or an item created from the GitHub section) starts a
  code review Chat of its PR, filed under the item, instead of the Start Chat
  dialog (see [Review PR launcher](pr-review.md)). Other items that merely
  link a PR keep the normal Start Chat. Right-click any PR-linked item for
  **Review**, **Fix CI**, **Open on GitHub**, and **Refresh PR Status**.
- **GitHub section.** The **Inbox** column (`⌥⌘L`, the toolbar's **Inbox**)
  shows a **GitHub** section for the project under its Linear panel: the sync
  bar (above), then **Review requested** and **My open PRs**, each foldable
  (click **GitHub** to fold the whole section; folds are remembered on this
  device). Lists load when the column opens and on ↻. The header's **All repos
  ▾** menu filters both lists to one repository of the project (and, with
  several projects, switches the project); ↑/↓, ↩, and Esc work in every menu
  of the column. With a destination, each requested PR shows **Synced ✓** once
  its item exists (click the row to reveal it) or **Pending first sync** until
  the next pass adds it; a PR some other item links shows **Linked**. Manual
  actions sit in each row's **⋯** menu: **Start review** (filed under the PR's
  item, else the destination), **Link to selected item**, and **Create work
  item** (under the destination, else the selected item); or drag the row onto
  an Outline item to create it there. Retries never duplicate.
- **PR review chats (⌘P / ⇧⌘P).** With a destination in the current project,
  the PR page's **Review chats** tab lists the destination's PR items (chip, Review, Fix CI) with their
  review Chats under each; click an item for its Details. Without one, the
  tab keeps its classified list of review Chats.
- If `gh` is missing or signed out, the surfaces say so (`gh auth login`). If
  the engine on this device has no PR review sync, they say **PR review sync
  isn't available on this host yet**.

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
