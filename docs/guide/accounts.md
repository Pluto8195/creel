# Accounts

By default Mast uses the same Claude Code and Codex logins as the CLIs on your machine. A project can use its own account instead, so work and personal projects stay apart, and two accounts can run at the same time.

## How it works

- Each project has a default account per agent (Claude Code, Codex).
- A chat picks the project's default when it first runs and keeps it. Change the default and only new chats change. Chats that already have an agent session stay on Default.
- A project account has its own home folder under Mast's data directory. It does not touch `~/.claude` or `~/.codex`. Skills and rules are shared into it, but credentials are not.
- The chat header shows which account a chat uses. The Start Chat dialog has an account chip for a one-chat override (it only shows when you have a project account).

## Add an account

1. Right-click the project and choose its account entry (it reads like "Claude account: Default…"), then **Add account…**.
2. Name it, and click **Continue**.
3. Mast starts the agent's sign-in (`claude auth login` or `codex login`). Click **Open** to see the page in your browser, or **Copy** the link. If the browser shows a code, paste it into the field and click **Submit**. Mast does not store the code.
4. When it says it is done, the account is the project's default for that agent.

**Cancel** stops the sign-in.

## Saved accounts

Logins you already added under **Settings → Accounts** show up in the project picker as saved logins. Choosing one moves it into a new project account. Two limits:

- The login you currently use as Default cannot be moved. Use **Add account** to sign in again.
- A moved login leaves the global switcher, and Mast stops refreshing it there, so two copies never fight over one token.

## Usage

**Settings → Project accounts** lists each account with its usage windows.

## Try it

1. Add an account to one project and sign in with a different login.
2. Start a chat in that project. Check the account chip in the chat header.
3. Start a chat in another project and check that it shows Default.
4. Run both at once.

<!-- TODO(Mikey): a screenshot of the Add account sheet and the account chip in a chat header. -->
