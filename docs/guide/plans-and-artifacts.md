# Plans and artifacts

Agents can publish plans, diagrams and option comparisons, and you read them in Mast instead of scrolling a chat. They are called artifacts. The format agents use is in [planning-artifact-conventions](../planning-artifact-conventions.md).

## Publishing

An agent calls `artifact_publish` with a file (Markdown, HTML, Mermaid, an image, or an options set) and a title. Mast stores it with the work item, in `.mast/artifacts/<item>/` under the project's checkout (kept out of git through `.git/info/exclude`), and links it in the chat. Publishing again with the same title adds a revision. An agent can pin one as the item's plan. Ask for one with a prompt like "Plan this, publish it as an artifact, and put the open decisions in as questions."

## Questions

A plan can hold question cards. Each has an id (`Q-1`), the question, and optionally an example and what it blocks.

- Every **blocking** question becomes its own Required Action on the item. The other open questions roll up into one "N open questions in <plan>" action.
- Answer in the card, or from the Required Action. Mast sends your answer to the chat that published the plan, marks the card answered and resolves the action. It does not edit the plan file. The agent applies your answer and publishes a new revision.

## Comments

On an HTML or Mermaid artifact, use **Comment on selection**, or **Pick element** to comment on a box or a node. The **Comments** panel lists them. If the thing you commented on is gone in a later revision, the comment shows as Outdated.

## Revisions and compare

When a plan has changed since you last looked, a chip says "Updated since you last viewed · rev 2 → 3". Click it to compare. Markdown compares side by side with changed blocks tinted. HTML, Mermaid and images compare side by side with synced scroll, plus **Highlight changes** (a pixel diff) and onion skin.

## Options and picking

An options artifact shows alternatives. **Side by side** puts 2–4 of them in equal columns that scroll and zoom together. Click **Pick this** on one, with an optional note. Mast tells the chat, resolves the related Required Action, and you can **withdraw** the pick.

## Reading

- Zoom: `⌘=`, `⌘−`, `⌘0`, or pinch. Zoom is remembered per artifact.
- Find: `⌘F`, then `⌘G` and `⇧⌘G`.
- Sections: a rail lists the headings once there are three or more. `⌥↑` / `⌥↓` jump between them.

## Try it

1. In a chat on an item, ask: "Plan how to add a dark mode toggle. Publish it as an artifact with two blocking questions."
2. Open the link. Check the two question cards and the new Required Actions.
3. Answer one in the card. Answer the other from Required Actions.
4. Ask the agent to apply your answers and republish.
5. Click the "Updated since you last viewed" chip and look at the compare view.

<!-- TODO(Mikey): a screenshot of a plan with question cards, and one of the compare view with Highlight changes. -->
