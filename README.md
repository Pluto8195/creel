<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/media/readme/mast-lockup-dark.png">
    <img src="docs/media/readme/mast-lockup-light.png" alt="Mast" width="320">
  </picture>
</p>

Run a team of coding agents from one outline of your work.

Mast is a native macOS app for running Claude Code and Codex (plus a few other agents) in chats, and for keeping those chats organised around the work they belong to. Your sessions stay on your machine. There is no Mast account.

## Why use it

You already run several agent chats at once. Mast gives them a place to live, so you can see what is running, what needs you, and what is done.

- **Spaces.** A Space is a project folder. Each Space has its own outline, instructions, accounts and settings.
- **Work Outline.** Nest chats under work items: a workstream, a ticket, a PR, an investigation. An item holds instructions that its chats inherit (Space → parent items → this item), where its chats run, and a status. Keys work everywhere, and `⌘Z` undoes a move. See [the outline guide](docs/guide/work-outline.md).
- **Required Actions.** When an agent needs a decision, an approval or missing information, it files a request on its item. The item shows it until you answer. Your reply goes back to the chat.
- **Chats on Claude Code and Codex.** Each chat shows the model it actually ran on and the account it uses.
- **Orchestrator and worker chats.** A coordinator chat can read its subtree, start worker chats on items below it, and collect their results through the Mast MCP tools. Workers finish with a "Ready to merge" action. See [orchestrating](docs/guide/orchestrating.md).
- **Plans you can review.** Agents publish plans, diagrams and option comparisons. You read them in the app, answer questions inline (blocking questions become Required Actions), comment on parts, zoom, compare revisions visually and pick an option. See [plans and artifacts](docs/guide/plans-and-artifacts.md).
- **PR and Linear sync.** A PR page buckets your PRs and review requests by what needs you. Review requests can appear as outline items on their own. Linear is opt-in per Space, read-only, and you add the tickets you want. See [GitHub and Linear](docs/guide/github-and-linear.md).
- **Per-project accounts.** Use a different Claude Code or Codex login in each project, and run two at once. See [accounts](docs/guide/accounts.md).
- **Side browser.** Web and PR links open beside the chat.
- **Agent tools.** Every chat gets the `mast-agent` MCP server. See [docs/mcp.md](docs/mcp.md).

<!-- TODO(Mikey): add real captures to docs/media/readme/ and uncomment.
![The Work Outline with chats under items](docs/media/readme/outline.png)
![The PR page](docs/media/readme/pr-page.png)
![A plan with visual compare](docs/media/readme/plan.png)
-->

## Install

Mast runs on macOS. It is built from source. Linux and Windows scripts are left over from upstream and are not tested.

You need:

- macOS with the Xcode command line tools (`xcode-select --install`).
- Rust through [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version.
- At least one agent CLI, signed in: [Claude Code](https://docs.claude.com/en/docs/claude-code) (`claude`) or Codex (`codex`).
- [`gh`](https://cli.github.com), signed in with `gh auth login`, for the PR page and PR sync.

```bash
git clone https://github.com/Pluto8195/mast.git
cd mast                                   # the default branch is work-outline-implementation
cargo build --profile installed -p mast   # optimised build, about 3 minutes the first time
```

The binary is `target/installed/zeron`. The binary and some internal names still say `zeron`; the product is Mast.

To get a normal app you can keep in `/Applications`, build the bundle and disk image:

```bash
scripts/package-macos.sh
```

It writes `target/package/Mast.app` and a `.dmg` next to it. The bundle is ad-hoc signed, so the first time you open it, right-click it and choose Open. Release file names keep the `zeron-` prefix on purpose.

To try it without touching an installed Mast, run the isolated dev app:

```bash
scripts/run-macos-dev.sh
```

It builds a debug binary and opens `Mast Dev.app` with its own data folder (`target/macos-dev/data`), bundle id and engine port. Set `MAST_DEV_DATA_DIR` or `MAST_DEV_IPC_PORT` to change them.

## First run

1. Open Mast. Add a project with `⇧⌘N`, or **New project…** in the project picker, and choose your repo folder.
2. Make sure an agent is signed in. Run `claude` or `codex` once in a terminal, or add an account for the project (see [accounts](docs/guide/accounts.md)).
3. Open the Outline with `⇧⌘L`, create an item with `⇧⌘I`, and start a chat in it with `⌘↩`.
4. Optional: for Linear, paste a personal API key in **Settings → Linear**, then switch on **Linear** in the project's Space details. See [GitHub and Linear](docs/guide/github-and-linear.md).

[Getting started](docs/guide/getting-started.md) walks through this with a first chat.

## Keys to learn first

| Key | Does |
| --- | --- |
| `⌘K` | Command palette |
| `⇧⌘L` | Open the Outline |
| `⇧⌘I` / `N` | New work item |
| `⌘↩` | Start a chat in the selected item |
| `⌘;` | Jump between a chat and its outline row |
| `⇧⌘↩` | Open the chat full screen |
| `⌘I` | Work Item Details for the selected row |
| `⌘Z` | Undo the last outline change |
| `⌘P` | PR page |
| `⌥⌘L` | Inbox column (Linear and GitHub) |
| `?` | Shortcut reference, in the outline |

The full list is in [docs/guide/keyboard.md](docs/guide/keyboard.md).

## Docs

Guides:

- [Getting started](docs/guide/getting-started.md)
- [The Work Outline](docs/guide/work-outline.md)
- [Orchestrating with coordinator and worker chats](docs/guide/orchestrating.md)
- [Plans and artifacts](docs/guide/plans-and-artifacts.md)
- [GitHub and Linear](docs/guide/github-and-linear.md)
- [Accounts](docs/guide/accounts.md)
- [Keyboard](docs/guide/keyboard.md)

Reference:

- [docs/work-outline.md](docs/work-outline.md): every key, menu and rule of the outline.
- [docs/pr-review.md](docs/pr-review.md): the review PR launcher.
- [docs/planning-artifact-conventions.md](docs/planning-artifact-conventions.md): how agents publish plans.
- [docs/mcp.md](docs/mcp.md): the MCP server that serves the agent tools.
- [ARCHITECTURE.md](ARCHITECTURE.md): how the engine and UI fit together.

## Credits and licence

Mast started as a fork of [zeronsh/zeron](https://github.com/zeronsh/zeron) (MIT). It was briefly called Creel, and it no longer tracks upstream. Third-party notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under the [MIT License](LICENSE).
