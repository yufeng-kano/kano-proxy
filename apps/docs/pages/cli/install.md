---
title: Install and sign in
description: Install the kano-proxy CLI on macOS, Linux or Windows, sign the machine in to your account, keep it updated, and sign it out.
---

# Install and sign in

The `kano-proxy` CLI does two jobs on your own machine: it sets up coding agents to use the proxy ([Set up agents](/cli/agents)), and it exposes a local model server as a provider ([Local models](/cli/local-models)). Both need the CLI installed and the machine signed in once.

## Install

Pick one:

::: code-group

```bash [Homebrew]
brew install yufeng-kano/tap/kano-proxy
```

```bash [Install script]
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
```

```powershell [Scoop (Windows)]
scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket
scoop install kano-proxy
```

:::

Homebrew works on macOS and Linux. The install script also works on both: it downloads the newest release for your platform, checks its checksum and installs to `~/.local/bin`. Every platform's archive and its checksums are also on the project's GitHub Releases page.

Check it:

```bash
kano-proxy --version
```

## Sign the machine in

```bash
kano-proxy login --base-url https://<your-domain>
```

1. The CLI asks for the **Server** and a **Device name**. The server is filled in from `--base-url`; the device name defaults to the machine's host name. The name is what the Access page shows.
2. It opens the sign-in page in your browser and also shows the link, so you can open it from any browser, for example when you are on a remote machine over SSH.
3. Sign in with Google if asked, check the device name, and choose **Approve**.
4. The page shows a code. Type it into the terminal. It works once and expires in 10 minutes.

You do this once per machine. The machine now appears under **Access** › **Devices** ([Access](/app/access#devices)). The CLI keeps its sign-in in `~/.config/kano-proxy/state.json` (`%APPDATA%\kano-proxy\state.json` on Windows), readable only by you. It holds a token that is replaced on every use, never an API key.

::: details Without the interactive screens
Every command runs with `--no-tui` and flags, for scripts and servers. Login then takes two runs:

```bash
kano-proxy login --no-tui --base-url https://<your-domain> --device-name box1
# open the printed link, approve, then:
kano-proxy login --no-tui --auth-code XXXX-XXXX
```

On a server or in a container, the state file must stay writable: the sign-in token changes on every refresh. Mount its directory as a writable volume and sign in once against it.
:::

## Update

```bash
kano-proxy update
```

It installs the newest CLI release for your platform after checking its checksum. If you installed with Homebrew or Scoop, it refuses and prints that manager's upgrade command instead: `brew upgrade kano-proxy` or `scoop update kano-proxy`.

## Check the state

```bash
kano-proxy status
```

It shows the device name and the server this machine is signed in to, whether the sign-in still works, each local endpoint and whether it is connected, and each coding agent the CLI set up.

## Sign out

```bash
kano-proxy logout
```

In order, it:

1. Restores every coding agent the CLI set up on this machine, the same as `kano-proxy agent restore` for each.
2. Revokes this device on the server. Every API key the device created is revoked with it.
3. Clears the sign-in and the list of local endpoints from the state file.

Backups of agent config files stay on disk. Add `--remove-group` to also delete the model groups the agents used. If you edited an agent's config since setup, the CLI shows the edits and asks before overwriting them; with `--no-tui` it needs `--force`.

Revoking the device from **Access** › **Devices** has the same effect on the server. The CLI on that machine then says the device was revoked and asks you to run `kano-proxy login` again.

## Commands

| Command | Does |
|---------|------|
| `kano-proxy login` | Sign this machine in |
| `kano-proxy logout` | Restore agents, revoke this device, forget the sign-in |
| `kano-proxy status` | Sign-in, tunnel and agent state |
| `kano-proxy update` | Install the newest CLI release |
| `kano-proxy agent setup\|restore\|list` | Coding agents: [Set up agents](/cli/agents) |
| `kano-proxy tunnel add\|attach\|remove\|list\|start` | Local models: [Local models](/cli/local-models) |

`kano-proxy <command> --help` lists every flag. Global flags: `--no-tui` (flags instead of interactive screens) and `--state <path>` (another state file).

In the interactive screens, Esc goes back, Esc twice exits, and Ctrl-C quits at once. Quitting changes nothing and prints `Cancelled — nothing was changed.`

The CLI never prints an API key and never logs request bodies.

## Related pages

- [Access](/app/access): devices, the keys they created, and the sign-in approval page.
- [Set up agents](/cli/agents)
- [Local models](/cli/local-models)
