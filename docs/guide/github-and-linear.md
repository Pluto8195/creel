# GitHub and Linear

Both are optional. Mast reads from them and never writes to Linear or GitHub. (Agents can, of course, use `gh` in their own chat, like any command.)

## The PR page

Press `⌘P`. It needs `gh` signed in (`gh auth login`). Tabs:

- **Authored by me**, **Review requested**: your PRs bucketed by what needs you, with CI, review threads and conflicts, and the Mast chats linked to each PR.
- **Review chats**: the chats started to review PRs.

On a row, **Review** starts a review chat through the Start Chat dialog and **Fix CI** starts one on a failing PR. If a review chat already exists, **Open review** opens it. `⇧⌘P` also opens the page. See the [review launcher](../pr-review.md).

## Synced PR items

Pick one outline item as the destination for review requests (right-click it, **Auto-sync PR Review Requests Here**, or use the GitHub section of the Inbox column). Every PR that asks for your review then becomes a child item with a "Review requested" Required Action. The item completes when you review it or the PR is merged or closed. Press `⌘↩` on one to start a review chat.

PR rows end in small icons: author, needs you, your review, CI, and the PR itself coloured by state. Hover for the full line. `?` has a legend.

## Linear

1. **Settings → Linear**: paste a personal API key and click **Save**. It goes in your Mac's Keychain.
2. Open the project's Space details and switch on **Linear**.
3. Press `⌥⌘L` to open the Inbox column. It lists your issues and projects.
4. Add the ones you want: **Create** on an issue makes an item "ENG-123 · title", or drag an issue onto an item. Nothing is added automatically unless you set a linked Linear item (below).

An issue item's title and description follow Linear until you edit them. The full ticket goes to the agent. PRs that Linear attaches to an issue show up on the item.

**Linked Linear item:** right-click an item and choose **Auto-add My Unstarted Linear Tickets Here**. Your assigned tickets that are not started and have no project then appear as its children, refreshed from a read-only query every 15 minutes. Choose **Stop Adding My Unstarted Tickets Here** to stop.

## Side browser

With **Open web links in Mast** on, PR, ticket and web links open in a pane beside the chat. A tab already open on that page is reused. `⌘`-click opens a link in your normal browser.

## Try it

1. Run `gh auth status` in a terminal.
2. Press `⌘P` and read the Authored tab.
3. On a PR, click **Review** and start the chat.
4. Right-click an outline item and set it as the PR sync destination. Wait about three minutes.
5. If you use Linear, connect it and add one issue by hand.

<!-- TODO(Mikey): a screenshot of the PR page and one of the Inbox column with GitHub and Linear sections. -->
