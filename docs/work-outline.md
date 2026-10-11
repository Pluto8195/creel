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
4. Click **Start Chat** on the selected row or in Details. The Chat launches with
   the item's effective instructions and execution setup, and it is associated
   automatically.
5. To bring existing Chats under the item, drag their rows onto it or run
   **Associate Chat with Work Item**.

Press `?` while the Outline has focus, or click the keyboard button in the
Outline toolbar, to open the shortcut reference. Hover over any Outline control
to see what it does and its current key.

## Keys

Outline keys work only while the Outline tree has focus. They never fire in the
inline editor, a picker, the search field, or any other text input.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Previous / next sibling | `↑` / `↓` | `↑` / `↓` |
| Parent / first child | `⌥↑` / `⌥↓` | `Alt+↑` / `Alt+↓` |
| Collapse / expand | `←` / `→` | `←` / `→` |
| Open Work Item Details | `↩` | `Enter` |
| Rename in place | `F2` | `F2` |
| New sibling / new child | `N` / `⇧N` | `N` / `Shift+N` |
| Move before previous / after next sibling | `⌥⇧↑` / `⌥⇧↓` | `Alt+Shift+↑` / `Alt+Shift+↓` |
| Indent (last child of the previous sibling) | `⇥` | `Tab` |
| Outdent (right after the former parent) | `⇧⇥` | `Shift+Tab` |
| Shortcut reference | `?` | `?` |

Every Work Outline command is also in the command palette and can be rebound in
**Settings → Shortcuts**. Only two commands ship with a global key, because
global keys also fire while a text field has focus:

| Command | Default |
| --- | --- |
| Open Outline | `⇧⌘L` / `Ctrl+Shift+L` |
| Create Work Item | `⇧⌘I` / `Ctrl+Shift+I` |
| Create Child Work Item, Rename, Move, Associate Chat with Work Item, Open Effective Instructions, Create Instruction from Chat, Open Required Actions, Start Chat in Work Item, Move Up / Down, Indent / Outdent, Show Outline Shortcuts | Unbound |

Commands that need a selected work item open the Outline and show a short notice
when nothing is selected. Tooltips and the shortcut reference always show the
current bindings, including keys you have rebound.

## Restructuring

Use the keys above, the matching palette commands, the **Move** picker (choose
**Place at Top** or **Bottom** with `←` / `→`), or drag and drop. Dropping on the
middle of a row makes the item the last child of that row. Dropping on the top
or bottom quarter places it before or after that row. Dropping a Chat row on a
work item associates the Chat with it. Descendants move with their item.
Restructuring never deletes anything. Archiving an item that has children asks
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
