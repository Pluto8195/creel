# Zeron → Mast: migration plan for what the code rename leaves alone

Status: plan only (piece 4 of branch `rename-zeron-to-mast`). Nothing here has been changed.
Pieces 1–3 of the branch renamed text, crates and runtime names (with aliases). This file covers the
parts that touch the installed app, live data, other devices or other people's repos. Each one needs
Mikey's go-ahead and, where marked **by hand**, his hands.

Order matters: do them in the order below. Each step is independent and reversible until the
"point of no return" noted in it.

## What the code rename already did (context)

- Crates, modules and types are `mast-*` / `mast_*`. The app package is `mast` (`cargo build -p mast`);
  the **binary is still named `zeron`**, so `target/installed/zeron` and
  `Contents/MacOS/zeron` are unchanged.
- Runtime names were renamed with the old name still accepted (see the piece 3 commit messages and
  the aliases table in the final report). Old data keeps working; new data is written with new names.

## 1. Binary name `Contents/MacOS/zeron` → `mast`

Where it shows up: `[[bin]] name` in `apps/mast/Cargo.toml`, `CFBundleExecutable` in
`dist/macos/Info*.plist`, `scripts/package-*.sh`, the update tarball layout (`update` crate looks for
`zeron` inside `Zeron.app`), `~/.zeron/app/current/zeron` (headless daemon), the launchd/systemd
units written by `zeron daemon install`, the `zeron mcp` command injected into agent MCP configs,
`/opt/zeron/bin/zeron`-style paths in user scripts.

Steps
1. Add `[[bin]] name = "mast"` and keep a second `[[bin]] name = "zeron"` (same `main.rs`) for one
   release so old launchd/systemd units, MCP configs of running agents and users' shell aliases still
   resolve.
2. Make the updater accept either filename inside the tarball (it already checks for a named file).
   Keep the tarball's internal `Zeron.app` directory name until every installed client is past the
   release that understands the new name (old clients unpack by that exact name).
3. `zeron daemon install` writes the new path; `daemon status` reports units that still point at
   `zeron` and says to re-run install.
4. Coordinator install procedure: `cp target/installed/mast …/Contents/MacOS/mast`, change
   `CFBundleExecutable`, re-sign. Because the Info.plist and binary must change together, install
   from a scripted step, not by hand.

Rollback: restore the previous binary from `~/zeron-dev-app-backups/` (the old binary reads the same
data). Keep the `zeron` shim binary for one release for exactly this.

Risks: the Dock/Launch Services cache may show the old executable name until `lsregister -f` runs;
Accessibility/Screen Recording grants are keyed on the bundle id + code signature, not the executable
name, so this step alone does **not** reset TCC.

By hand: restart Mast after install (never done by an agent).

## 2. Bundle id `sh.zeron.app.dev` (dev) and `sh.zeron.app` (release), URL scheme `zeron://`

Where: `dist/macos/Info*.plist` (`CFBundleIdentifier`, `CFBundleURLSchemes`,
`sh.zeron.app.conversation`), `notify.rs` (`MACOS_BUNDLE_ID`), `update/src/lib.rs`
(`launchctl kickstart gui/<uid>/sh.zeron.app`), `daemon.rs` (`LAUNCHD_LABEL`), codesign
`--identifier` in install steps and `scripts/run-macos-dev.sh`.

Consequences of changing the bundle id (this is the expensive one)
- **TCC permissions reset**: Screen Recording, Accessibility, Automation, Notifications, Files &
  Folders, microphone and any "Full Disk Access" grants are keyed on bundle id. After the change
  Mast asks again for each, and the old `Mast.app` entries remain as stale rows in System Settings.
- **Keychain ACLs**: items created by the old app list the old signing identity as trusted. The
  new bundle id gets a "Mast wants to use your confidential information" prompt per item
  (Linear key, agent accounts) until "Always Allow" is clicked again.
- **UserDefaults / saved window state / notification history** under the old id's
  `~/Library/Preferences/sh.zeron.app.dev.plist` and `Saved Application State` are not read.
- **Login item / launchd label** changes; the old label keeps running unless booted out.
- `zeron://open/chat/...` links already pasted into notes keep working only while the old scheme is
  still declared in the plist.

Steps
1. Choose the new ids: `sh.mast.app` / `sh.mast.app.dev` (needs a domain/identity decision; see
   "Decisions needed").
2. Declare **both** URL schemes (`mast`, `zeron`) in `CFBundleURLSchemes`; generate `mast://`,
   accept both in `parse_mast_conversation_link`.
3. In the same release, copy the old preferences domain once (`defaults export/import`, run by the
   app on first launch when the new domain is empty).
4. Install, re-grant the TCC permissions (list them in a release note), re-approve keychain items.
5. `launchctl bootout gui/<uid>/sh.zeron.app` and remove the old plist after verifying the new label.

Rollback: reinstall the previous binary and sign with `--identifier sh.zeron.app.dev`. TCC grants
for the old id are still there (macOS keeps them), so rollback is painless. This is why it can be
undone, but not why it is cheap.

By hand: every permission prompt; `System Settings → Privacy & Security` cleanup of stale rows;
Apple Developer / signing identity if a registered App ID is involved (the dev build uses ad-hoc
signing, so none today). Never run two bundles at once (they share `~/.zeron`).

Do this last among the macOS items, in the same release as the binary rename, so TCC resets only once.

## 3. Data directory `~/.zeron` (and `~/.zeron-dev`, `~/.zeron/worktrees`, `~/.zeron/app`, `~/.zeron/env`)

Holds the live stores: registry, session docs, Linear cache, logs, sound/theme settings,
`device-id`, `session.json`, update staging, default worktree root.

Recommended approach: **do not move it; alias it.** A move is where data is lost.
1. Release N: new installs create `~/.mast`; an existing `~/.zeron` is detected and **kept in
   place** (`data_dir()` returns `~/.zeron` if it exists and `~/.mast` does not). Nothing moves.
2. Optional release N+1: offer "Move data to ~/.mast" as an explicit action in Settings:
   quit engine → take the instance lock → `rename(2)` `~/.zeron` to `~/.mast` (same volume,
   atomic) → write a symlink `~/.zeron -> ~/.mast` so old binaries, scripts, launchd units and the
   `worktrees` paths recorded in the registry (absolute paths!) still resolve.
3. The symlink stays until a second cleanup release.

Why a symlink, not a path rewrite: the registry and session docs store absolute worktree paths
under `~/.zeron/worktrees/...`, and chats' git worktrees have `.git` files pointing back into it.
Rewriting those is a data migration across CRDT docs on every device; the symlink makes it
unnecessary.

Rollback: remove the symlink, `mv ~/.mast ~/.zeron`. Safe only while Mast is not running.

Risks
- Old binary running at the same time as the new one on the same dir: the instance lock
  (`instance_lock.rs`) protects the dir, but not across a rename in flight; do it with Mast quit.
- `ZERON_DATA_DIR` / `MAST_DATA_DIR` both honoured (piece 3), so dev data dirs keep working.
- Time Machine / backup tools see a new path; irrelevant after a rename but note it.
- The `~/.zeron-dev` dev dir: same treatment, separately.

By hand: run the move action with Mast quit, verify, delete the symlink later.
Agents must still treat `~/.zeron` as live data until this ships (hard rule).

## 4. Keychain service names

- `sh.zeron.linear` / account `api-key` (Linear API key) in `engine/src/linear/credentials.rs`.
- Claude/Codex credentials use the **providers'** items and are not ours; do not touch.

Steps: read `sh.mast.linear` first, fall back to `sh.zeron.linear`; on a successful fallback read,
write the key to the new service and leave the old item (do not delete in the same release).
Delete the old item in a later release, behind a Settings action.

Rollback: nothing to undo, since the old item is never removed.
Risk: the first read of each item by the new bundle id prompts (see §2). If §2 has not shipped, the
bundle id is unchanged and there is no extra prompt. By hand: click "Always Allow".

## 5. Cross-device sync with mixed versions

What crosses devices: Loro registry/session docs, device RPC (method names, JSON field names), the
edge worker (`edge/`, `edge.zeron.sh`), update manifests.

Rules used by the code rename and to keep for any follow-up:
- **Wire names are not renamed** (JSON field names such as `zeronOwned`, enum strings such as
  `zeronUpgradeRequired`, RPC method names, doc container names, the hashed instruction preamble
  `zeron_work_outline_instructions`, the idempotency-key namespace). An old peer cannot read a new
  name; a new peer can always read an old one. If a wire name must change: add the new name as a serde
  alias first (release N, reading both, writing old), switch writing in release N+1 after all
  devices updated, drop the alias never.
- Files and links people paste (`zeron-artifact:`, `.zeron/` folders) are read in both spellings
  forever.
- The edge hostname `edge.zeron.sh` and `zeron.sh` stay (DNS/ownership is upstream's, see below);
  `DEFAULT_EDGE_URL` is the only code reference.

Risk: a device that has not updated keeps writing the old names (`.zeron` folder, `zeron-artifact:`
links); the new reader accepts them, so mixed fleets converge. The reverse (old reader, new writer)
only happens for things in "Not backward-compatible on purpose" below.

## 6. The `zeron/` submodule folder and the GitHub repository layout

Today: parent repo `agent-mode-tools`, branch `zeron-work-outline`, submodule path `zeron`
(`.gitmodules` url `https://github.com/Pluto8195/mast.git`), worktrees under
`.worktrees/zeron/<branch>`, Work Outline instructions and the HANDOFF all say `zeron/`.

Steps
1. `git mv zeron mast` in the parent repo, update `.gitmodules` `[submodule "mast"] path = mast`,
   `git submodule sync`, move `.git/modules/zeron` → `.git/modules/mast` (or leave the module dir; it
   is internal) and fix its `core.worktree`.
2. Move `.worktrees/zeron` → `.worktrees/mast` after pruning all worktrees (`git worktree prune`
   afterwards; worktree `.git` files hold absolute paths, so recreate rather than move).
3. Update the Work Outline root instructions, HANDOFF, memory notes and every script that says
   `zeron/` or `.worktrees/zeron`.
4. Re-run `bind_chat_worktree` for any chat whose bound path moved.

Rollback: reverse `git mv`; since it is a rename commit in the parent, `git revert` works.
Risks: every open chat/worktree bound to an old path breaks (do it when no children are running);
absolute paths recorded in the registry for worktrees under `.worktrees/zeron` go stale.
By hand: Mikey picks the moment (no running children), then updates the coordinator root
instruction text.

GitHub: the repo is already `Pluto8195/mast`; there is nothing to rename. Branch names stay
(`work-outline-implementation`). `zeronsh/zeron` remains the `upstream` remote.

## 7. Upstream `zeronsh/zeron` merges

The code rename makes `git merge upstream/main` conflict on nearly every file that mentions a crate
path or identifier. Keep it manageable:

1. Keep `scripts/rename-map.sh` (to be added when this branch is merged; the mapping is the set of
   `perl` substitutions used in pieces 2–3: crate names `zeron-X`/`zeron_X` → `mast-X`/`mast_X`,
   `apps/zeron` → `apps/mast`, the `Zeron` MCP type, listed identifiers).
2. To merge upstream: create a throwaway branch from `upstream/main`, run the script on it (also
   `git mv` the three renamed paths), commit "upstream, renamed", then merge **that** branch into
   ours. Conflicts then only reflect real divergence.
3. Alternatively cherry-pick specific upstream commits through the same script (`git format-patch`
   then `rename-map.sh` over the patch text then `git am`).
4. Record the upstream commit that was last imported (tag `upstream-imported-YYYY-MM-DD`) so the next
   run only has to map new commits.
5. Because we no longer open PRs upstream, nothing needs to flow back.

Cost estimate: one scripted merge per upstream release; hand conflicts only in files we changed
(Work Outline, artifacts, Linear, UI shell). The longer we wait, the more the script's mapping drifts,
so keep the tag current.

## Not backward-compatible on purpose

- New artifact folders (`.mast/`) are invisible to an old Mast that has not updated: the old app does
  not look there. Mixed-version devices sharing one repo will each see only the artifacts written by
  their own spelling until both update (reads accept both, so updated devices see everything).
- New agent runs get the MCP server `mast-agent`; an old build's transcripts mention
  `mcp__zeron-agent__…` and keep rendering, but the tools are only registered under the new name for new runs.
  Agents already mid-run keep their original config.

## Decisions needed from Mikey

1. New bundle id / label domain (`sh.mast.app`? something under cofactr?).
2. Keep a `zeron` shim binary for how many releases?
3. Move `~/.zeron` at all, or alias it forever (recommended: alias, offer the move later).
4. When to rename the `zeron/` submodule folder (needs a window with no running children).
5. Whether `edge.zeron.sh` / `zeron.sh` should be replaced by a domain we control.

## Do not touch until approved

`~/.zeron`, `sh.zeron.app.dev`, `sh.zeron.linear`, `Contents/MacOS/zeron`, the `zeron/` submodule
path, the `Zeron.app` directory inside update tarballs, `zeron://` registration.
