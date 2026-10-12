---
title: 安裝與登入
description: 在 macOS、Linux 或 Windows 安裝 kano-proxy CLI，讓這台機器登入你的帳號，保持更新，以及登出。
---

# 安裝與登入

`kano-proxy` CLI 在你自己的機器上做兩件事：把 coding agent 設定成走 proxy（[設定 agent](/zh-TW/cli/agents)），以及把本機的模型伺服器變成 proxy 上的供應商（[本機模型](/zh-TW/cli/local-models)）。兩者都需要先安裝 CLI，並讓這台機器登入一次。

## 安裝

擇一即可：

::: code-group

```bash [Homebrew]
brew install yufeng-kano/tap/kano-proxy
```

```bash [安裝腳本]
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
```

```powershell [Scoop（Windows）]
scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket
scoop install kano-proxy
```

:::

Homebrew 支援 macOS 和 Linux。安裝腳本也是：它會下載你這個平台的最新版本，核對 checksum 後安裝到 `~/.local/bin`。各平台的壓縮檔和 checksum 也都放在專案的 GitHub Releases 頁面。

確認安裝成功：

```bash
kano-proxy --version
```

## 讓機器登入

```bash
kano-proxy login --base-url https://<your-domain>
```

1. CLI 會詢問 **Server** 和 **Device name**。Server 會帶入 `--base-url` 的值；裝置名稱預設是這台機器的主機名稱，也就是 Access 頁上顯示的名字。
2. CLI 會在瀏覽器開啟登入頁，同時把連結印出來。用 SSH 連到遠端機器時，可以在任何一台電腦的瀏覽器打開這個連結。
3. 需要的話先用 Google 登入，確認裝置名稱後按 **Approve**。
4. 頁面會顯示一組代碼，把它輸入終端機。代碼只能用一次，10 分鐘後失效。

每台機器只需要做一次。完成後，這台機器會出現在 **Access** › **Devices**（[Access](/zh-TW/app/access#devices)）。CLI 把登入狀態存在 `~/.config/kano-proxy/state.json`（Windows 是 `%APPDATA%\kano-proxy\state.json`），只有你自己能讀。裡面存的是每次使用都會更換的 token，不是 API key。

::: details 不用互動畫面
每個指令都可以加 `--no-tui`，改用參數執行，適合腳本和伺服器。這時登入要分兩次執行：

```bash
kano-proxy login --no-tui --base-url https://<your-domain> --device-name box1
# 打開印出的連結並核准，然後：
kano-proxy login --no-tui --auth-code XXXX-XXXX
```

在伺服器或容器裡，state 檔必須保持可寫入，因為登入 token 每次更新都會換。把它所在的目錄掛成可寫入的 volume，再對它登入一次。
:::

## 更新

```bash
kano-proxy update
```

它會下載你這個平台最新的 CLI 版本，核對 checksum 後安裝。如果你是用 Homebrew 或 Scoop 安裝的，它會拒絕更新，改為印出對應的指令：`brew upgrade kano-proxy` 或 `scoop update kano-proxy`。

## 查看狀態

```bash
kano-proxy status
```

會顯示這台機器的裝置名稱、登入的伺服器、登入是否仍然有效、每個本機端點的連線狀態，以及 CLI 設定過的每個 coding agent。

## 登出

```bash
kano-proxy logout
```

依序執行：

1. 還原 CLI 在這台機器上設定過的每個 coding agent，效果等同對每個 agent 執行 `kano-proxy agent restore`。
2. 在伺服器上撤銷這台裝置。它建立的 API key 會一起被撤銷。
3. 從 state 檔清除登入資訊和本機端點清單。

agent 設定檔的備份會留在磁碟上。加上 `--remove-group` 會連 agent 用的 model group 一起刪除。如果你在設定之後手動改過 agent 的設定檔，CLI 會列出改過的地方，覆寫前先問你；用 `--no-tui` 時必須加 `--force`。

在 **Access** › **Devices** 撤銷裝置，對伺服器的效果相同。那台機器上的 CLI 之後會告訴你裝置已被撤銷，請你重新執行 `kano-proxy login`。

## 指令一覽

| 指令 | 作用 |
|------|------|
| `kano-proxy login` | 讓這台機器登入 |
| `kano-proxy logout` | 還原 agent、撤銷這台裝置、清除登入資訊 |
| `kano-proxy status` | 登入、tunnel 和 agent 的狀態 |
| `kano-proxy update` | 安裝最新的 CLI 版本 |
| `kano-proxy agent setup\|restore\|list` | coding agent：[設定 agent](/zh-TW/cli/agents) |
| `kano-proxy tunnel add\|attach\|remove\|list\|start` | 本機模型：[本機模型](/zh-TW/cli/local-models) |

`kano-proxy <指令> --help` 會列出所有參數。全域參數有 `--no-tui`（用參數取代互動畫面）和 `--state <路徑>`（使用另一個 state 檔）。

在互動畫面中，Esc 回上一步，連按兩次 Esc 離開，Ctrl-C 立即結束。中途離開不會改動任何東西，並印出 `Cancelled — nothing was changed.`。

CLI 不會印出 API key，也不會記錄請求內容。

## 相關頁面

- [Access](/zh-TW/app/access)：裝置、裝置建立的 key，以及核准登入的頁面。
- [設定 agent](/zh-TW/cli/agents)
- [本機模型](/zh-TW/cli/local-models)
