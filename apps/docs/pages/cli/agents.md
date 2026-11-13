---
title: Set up agents
description: Point Claude Code or the Codex CLI at Kano Proxy with kano-proxy agent setup, start one for a single session with agent launch, check what setup wrote with agent list, and undo it with agent restore.
---

# Set up agents

`kano-proxy agent setup` points a coding agent on this machine at your proxy in one step, and `kano-proxy agent restore` undoes it exactly. `kano-proxy agent launch` starts the agent against the proxy for one session instead, without changing its config. The CLI supports **Claude Code** (`claude-code`) and the **Codex CLI** (`codex`). For Cursor, Cline and OpenCode, follow the manual steps on their pages under Coding agents.

The machine must be signed in first ([Install and sign in](/cli/install)).

## Set up

```bash
kano-proxy agent setup claude-code
kano-proxy agent setup codex
```

The command reads the models your account can serve, the same list as the Models page, and lets you choose which models answer the agent. Then it:

1. Creates or updates a model group named `claude-code` or `codex-cli`. Its model names are plain ids the agent knows, such as `claude-opus-5` (an example), and each name sends requests to the targets you picked.
2. Creates an API key named `<agent>@<device name>`, for example `claude-code@my-mac`. On a rerun it revokes the key it created last time and creates a new one.
3. Backs up the agent's config file, the first time only, then writes the proxy's URL, the key and the models into it.
4. Checks that the group endpoint lists the names it just wrote. This reads the model list only; it sends no model request.

Nothing is invented. Claude Code needs at least one model on your account, from any provider. The Codex CLI needs a connected Codex (ChatGPT) account, because the name Codex starts with must be one of its own models. Without them the command says so and stops instead of writing model names that would fail.

What each agent's config gets, field by field, is on its own page: [Claude Code](/agents/claude-code), [Codex CLI](/agents/codex-cli).

::: details Why a model group
Coding agents decide behaviour from the model name they see. Claude Code enables features and fills its Fable, Opus, Sonnet and Haiku slots from real Claude ids. The Codex CLI reads context windows and effort levels from its own catalog, which knows `gpt-5.6-sol` but not `codex/gpt-5.6-sol`. A group endpoint serves those plain names and routes each to whatever you picked, from any provider. So the agent sees the name it expects while the proxy can answer with another provider first and fall back to the next target when one is unavailable. The group is a normal group: change its targets later on the [Groups](/app/groups) page without running the CLI again.
:::

### Choosing models

The setup screen opens on a list of everything it will write, with the current value of each item. A rerun starts from what is in place now.

- **Claude Code**: one row per slot (Fable, Opus, Sonnet, Haiku). Each slot shows the name Claude Code sees and the models that answer it, in order. Haiku is also where Claude Code runs background work such as session titles, so keep it on a cheap model.
- **Codex CLI**: the model Codex starts with, its reasoning effort (`low`, `medium`, `high` or `xhigh`; default `high`) and the models that answer behind it, in order.

Keys on the screen:

- Enter opens an item. In a model list, Space turns a model on or off; the order you turn them on is the order they are tried, shown as `[1]`, `[2]` and so on.
- Enter applies the change and goes back to the list; Esc goes back without it.
- Changed rows are marked `(changed)`. Nothing is written until you press **Tab** on the list, which saves everything at once.
- Esc twice exits. With unsaved changes it first asks `Save changes?`: Y saves, N exits without saving, Esc goes back.

### Without the interactive screen

```bash
kano-proxy agent setup claude-code --no-tui \
  --opus claude-code/claude-opus-5 \
  --haiku claude-code/claude-haiku-4-5

kano-proxy agent setup codex --no-tui --model gpt-5.6-sol --effort high
```

The model ids above are examples; use ids from your Models page.

- Claude Code: `--fable`, `--opus`, `--sonnet` and `--haiku` each take a comma-separated list of `provider/model` ids, in the order tried. A bare id means `claude-code/<id>`. Give at least one slot.
- Codex: `--model` is the plain id Codex starts with, `--effort` its reasoning effort, and `--target` (repeatable, in order) a model that answers behind it. A bare target means `codex/<id>`. Without `--target` the model itself answers.

Add `--dry-run` to either command to print every field it would write and the group it would create or change, without writing, creating a key or changing the group. The key shows as `<new key>`.

## Start an agent for one session

```bash
kano-proxy agent launch claude-code
kano-proxy agent launch codex
```

`kano-proxy agent launch` starts the agent against your proxy for this one session and leaves its config files alone. `agent setup` is the permanent way: it changes the agent's config, so every later start goes through the proxy. `agent launch` writes nothing to the agent's config, and the next time you start the agent on its own it works as before. In order, it:

1. Uses the agent's model group (`claude-code` or `codex-cli`) and changes nothing it already holds. Models of the agent's own provider that the group has no name for, such as ones your account gained since the group was written, are added under their own names, and the launch says which. If your account has no such group yet, it creates one with the defaults setup starts from and says so. Creating one needs a Claude model on your account for Claude Code, or a connected Codex (ChatGPT) account for the Codex CLI. Without one the command says which is missing and stops.
2. Creates an API key for this session only, named `<agent>-launch@<device name>`, for example `claude-code-launch@my-mac`.
3. Starts the agent in the current terminal and directory.
4. When the agent exits, revokes the key and deletes the files it made for the session. It exits with the agent's exit code.

Settings the agent itself saves during the session stay, as in any session, for example a default model picked with `/model`.

A launch key has no spend limit. A limit you set on the setup key on the [Access](/app/access) page does not apply to launches.

Everything after `--` is passed to the agent unchanged:

```bash
kano-proxy agent launch claude-code -- -p "explain this repo"
kano-proxy agent launch codex -- exec "run the tests"
```

::: details How the session reaches the proxy
Claude Code gets a settings file of the launch's own through `--settings`, which ranks above your user, project and local settings files. The Codex CLI gets `-c` overrides on its command line, which rank above `config.toml`, and the key in its environment, never on the command line. So a config that already names another gateway or provider does not get in the way. Managed (enterprise) settings rank higher still, and a launch cannot override them.

If your Claude Code settings save a default model the group cannot serve, the session starts on the Opus slot and the launch says so. If your Codex `config.toml` sets an effort the proxy refuses (`minimal` or `ultra`), the launch uses `high` and says so.
:::

### Options

Put these before `--`:

- `--dry-run` prints the group it would use or create, the key name and the command line, then exits without creating a group or a key, or starting anything. The key shows as `<new key>`.
- `--model <name>`, Codex only: the model Codex starts with. It must be a name the group serves.
- `--effort <low|medium|high|xhigh>`, Codex only: the reasoning effort for this session. Without it Codex's own setting applies.

For Claude Code, pass the model to Claude Code itself: `kano-proxy agent launch claude-code -- --model <name>`. For Codex, a `-c` or `-m` you pass after `--` wins over the launch's value for the same setting.

### What a launch refuses

- Claude Code `--settings`: the launch passes its own settings file, and two would conflict.
- Claude Code `--bg` and `--background`: a background session outlives the launch and would still need the key after it is revoked. For the same reason a launched Claude Code session cannot be backgrounded.
- Codex `--oss`, `--local-provider` and `--remote`: they take Codex off the proxy.

### Resume a Codex session

Codex lists past sessions by provider, and a launch uses a provider of its own. Resume a launched session through the launch, not with a plain `codex resume`:

```bash
kano-proxy agent launch codex -- resume
```

### If a launch is killed

A launch that is killed before the agent exits, for example with `kill -9` or a power loss, cannot revoke its key. The key then stays valid until the next `kano-proxy agent launch`, which revokes it and deletes the leftover files. `kano-proxy status` shows how many launches are running and how many did not exit cleanly. `kano-proxy logout` also revokes the key, together with every other key the device created.

If the server cannot be reached when the agent exits, the launch says so and the next launch revokes the key.

## Check what is set up

```bash
kano-proxy agent list
```

One row per agent: whether this machine set it up, the group, the key's name, when it was written, and whether the config file still holds what the CLI wrote:

- `ok`: unchanged.
- `drifted`: you or the agent changed a field the CLI wrote.
- `missing`: the config file is gone.

It makes no server call. A launch is not a setup and has no row. `kano-proxy status` shows the same rows together with the sign-in state.

## Undo

```bash
kano-proxy agent restore claude-code
```

By default only the fields setup wrote change back: a field that existed before gets its old value, one that did not is removed. Everything else in the file stays, including what the agent wrote since, such as a default model Claude Code saved with `/model`. For Codex the `[model_providers.kano]` table is removed and the generated `kano-models.json` is deleted, or put back from its backup if you had one before.

On the server, the key setup created is revoked. The model group stays, because other machines or your own tools may use its endpoint. Add `--remove-group` to delete it too.

Options:

- `--full` puts the whole config file back from the backup taken before the CLI first wrote to it. Every later change to the file is lost, so the CLI shows the difference and asks first. It backs up the current file again before replacing it. `--backup <path>` names a different backup.
- `--dry-run` prints the plan and changes nothing.
- `--force` skips the confirmations; with `--no-tui` it is required when fields drifted or with `--full`.

If you edited a field the CLI wrote, restore lists it and asks before overwriting it. It never reverts a hand edit silently.

`kano-proxy logout` restores every agent this machine set up before it signs the machine out.

## Where the files are

| | macOS and Linux | Windows |
|-|-----------------|---------|
| Claude Code config | `~/.claude/settings.json` | `%USERPROFILE%\.claude\settings.json` |
| Codex CLI config | `~/.codex/config.toml` and `~/.codex/kano-models.json` | `%USERPROFILE%\.codex\` |
| Backups | `~/.config/kano-proxy/backups/<agent>/` | `%APPDATA%\kano-proxy\backups\` |
| Files of a running launch | `~/.config/kano-proxy/launch/` | `%APPDATA%\kano-proxy\launch\` |

The CLI only writes Claude Code's user settings file, never a project's `.claude/settings.json`, which may be committed. Backups are readable only by you and the CLI never deletes them. A launch's files are readable only by you too, and are deleted when the session ends.

## Related pages

- [Claude Code](/agents/claude-code), [Codex CLI](/agents/codex-cli): the same configuration by hand, and each tool's limits.
- [Groups](/app/groups): the `claude-code` and `codex-cli` groups.
- [Access](/app/access): the keys setup and launches created, under their device.
