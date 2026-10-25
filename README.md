<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/media/readme/creel-lockup-dark.png">
    <img src="docs/media/readme/creel-lockup-light.png" alt="Creel" width="320">
  </picture>
</p>

<p align="center"><em>English | <a href="README.zh-CN.md">简体中文</a></em></p>

Run a team of coding agents from one outline of your work.

Creel is a native macOS app that runs Claude Code, Codex, Cursor and other coding agents (Devin, Grok, Hermes, Pi, Antigravity) in chats, and organises those chats around the work they belong to. Sessions are stored on your machine. There is no account and no network requirement.

<!--
## Screenshots

Drop captures into docs/media/readme/ and uncomment:

![The Work Outline with chats under items](docs/media/readme/outline.png)
![The PR page](docs/media/readme/pr-page.png)
![A plan with visual compare](docs/media/readme/plan.png)
-->

## What it does

- **Work Outline.** Nest chats under work items: a workstream, a ticket, a PR, an investigation. Items hold instructions that their chats inherit, the repos and worktrees they run in, and a status. Everything is keyboard-driven. See [docs/work-outline.md](docs/work-outline.md).
- **Child and coordinator chats.** A coordinator chat can start child chats on its own item or a descendant, read the subtree, and collect results. Children report back through their Required Actions.
- **Required Actions.** Agents ask for a decision, approval or missing information. The request stays on its item until you answer it, and your reply goes back to the chat.
- **Item instructions.** Per-item directives, with revisions, delivered to Claude Code and Codex on every run. Skill autocomplete in the editor.
- **PR review page.** Your authored PRs and review requests in one place, bucketed by what needs you, with CI, threads and conflicts. Start a review chat per PR, or open the one that exists. Synced PRs show up as outline items. The review launcher is described in [docs/pr-review.md](docs/pr-review.md).
- **Planning artifacts.** Agents publish plans and diagrams (Markdown, HTML, Mermaid, images) that you read in the app. Revisions compare visually: side by side with synced scroll, pixel-diff highlights, onion skin, rendered Markdown with changed blocks tinted. Find, a section index, and question cards for open decisions. Conventions for agents are in [docs/planning-artifact-conventions.md](docs/planning-artifact-conventions.md).
- **Linear.** Opt in per Space. An inbox lists your issues and projects, and you add the ones you want to the outline by hand. The full ticket goes to the agent. Creel never writes to Linear.
- **Per-chat diff targets.** The Changes pane follows the worktree an agent created (`bind_chat_worktree`), or a branch or PR you pick, so you review the agent's work and not the repo root.
- **Agent tools.** Every chat gets the `zeron-agent` MCP server: read and update its work item, create children, ask for Required Actions, bind a worktree, publish artifacts, start and read other chats. See [docs/mcp.md](docs/mcp.md).
- **Side browser.** Web and PR links open beside the chat.

## Build and run (macOS)

Creel is built from source. You need macOS, Xcode command line tools, and Rust through [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`).

```bash
git clone https://github.com/Pluto8195/creel.git
cd creel
git checkout work-outline-implementation   # the default branch

cargo build -p zeron                       # debug binary at target/debug/zeron
```

The binary and crate are still called `zeron`; only the product name changed.

Run it as an isolated dev app:

```bash
scripts/run-macos-dev.sh
```

This builds the debug binary, wraps it in `target/macos-dev/Creel Dev.app`, signs it (with an Apple Development identity if you have one, ad-hoc otherwise) and opens it. The dev app has its own data directory (`target/macos-dev/data`), bundle id and engine port, so it can run next to an installed Creel. Set `ZERON_DEV_DATA_DIR` or `ZERON_DEV_IPC_PORT` to change them.

To build an app bundle and disk image:

```bash
scripts/package-macos.sh
```

This builds a release binary and writes `target/package/Creel.app` plus a `.dmg` and an updater tarball next to it. The bundle is ad-hoc signed unless you set `CODESIGN_IDENTITY`. On first launch, right-click → Open. Release file names keep the `zeron-` prefix on purpose.

The engine and its sync code come from upstream and are still in the tree, but Creel is meant to be used local-only. Linux and Windows packaging scripts are still in `scripts/`, but Creel is developed and tested on macOS only.

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md): how the engine and UI fit together.
- [docs/work-outline.md](docs/work-outline.md): the Work Outline, keys, Linear, PR review sync.
- [docs/pr-review.md](docs/pr-review.md): the review PR launcher.
- [docs/planning-artifact-conventions.md](docs/planning-artifact-conventions.md): how agents publish plans.
- [docs/mcp.md](docs/mcp.md): the `zeron mcp` server that serves the agent tools.

## Credits and licence

Creel started as a fork of [zeronsh/zeron](https://github.com/zeronsh/zeron) (MIT) and no longer tracks it. Third-party notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under the [MIT License](LICENSE).
