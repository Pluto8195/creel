# Getting started

This gets you from a fresh install to a first chat running inside a work item.

## Install

Follow the install steps in the [README](../../README.md). You need Rust, the Xcode command line tools, a signed-in agent CLI (Claude Code or Codex) and `gh`.

## Add a project

A project (Mast also calls it a Space) is a folder with your code in it, usually a git repo.

1. Press `⇧⌘N`, or open the project picker and choose **New project…**.
2. Pick the folder.

The project's chats, outline, instructions and accounts all hang off this folder.

## Start a chat

Press `⌘N` for a new session in the composer, pick a model, and send a message. This works without any work item. The outline is for when you want to keep track of more than one chat.

## The outline in five minutes

1. Press `⇧⌘L` to open the Outline.
2. Press `⇧⌘I` (or `N`), type a title such as "Fix login bug", and press Enter.
3. Select the item and press `↩` to open **Work Item Details**. Under **Instructions**, add a line such as "Run the tests before you say you're done." and click **Save**.
4. Press `⌘↩`. The Start Chat dialog opens. Write the prompt and click **Start Chat**. The chat starts with the item's instructions and sits under the item.
5. Press `⌘;` to jump between the chat and its row in the outline. Press `⇧⌘↩` to open the chat full screen.

When the agent needs you, the item shows **User Action Required**. Open **Required actions** in the toolbar and answer it.

## Try it

1. Add a project and open the Outline.
2. Make an item with an instruction.
3. Start a chat in it and ask the agent to read the repo and summarise it.
4. Press `?` in the outline to see every key.
5. Right-click the row. Every entry shows its key.

<!-- TODO(Mikey): a 60-second screen recording of steps 1-4 would be the best intro to this page. -->

## Next

- [The Work Outline](work-outline.md)
- [Orchestrating](orchestrating.md)
- [Keyboard](keyboard.md)
