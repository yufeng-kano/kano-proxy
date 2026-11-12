---
title: Claude Code
description: 用一道 CLI 指令，或手動用 ANTHROPIC_BASE_URL 與 ANTHROPIC_AUTH_TOKEN 把 Claude Code 指向 Kano Proxy，並選擇由哪個 provider/model 回應。
---

# Claude Code

完整可用。Claude Code 用的是 Anthropic Messages API，proxy 原生支援，而且任何已連接的供應商都能回應，不限於 Claude。

## 用 CLI 設定

先裝好 [kano-proxy CLI](/zh-TW/cli/install)：

```console
$ kano-proxy login                      # 每台機器一次
$ kano-proxy agent setup claude-code
```

1. 替 Claude Code 的每個槽位（Fable、Opus、Sonnet、Haiku）依序選 proxy 上由哪些模型回答。任何供應商都可以。第一個負責回答，它被暫停或額度用完時，由後面的依序接手。
2. CLI 會建立名為 `claude-code` 的模型群組，建立一把以這台機器命名的 API key，再把指向這個群組的 `env` 區塊寫進 `~/.claude/settings.json`。
3. 啟動 `claude`。

Haiku 槽位請放便宜的模型：Claude Code 的背景工作（例如產生 session 標題）也跑在這裡。

::: details 為什麼 CLI 要透過模型群組
Claude Code 會依它看到的模型 id 決定開啟哪些功能。群組端點 `/g/claude-code/anthropic` 回的是 `claude-opus-5` 這種原本的 Claude id，就算 proxy 實際轉給別的供應商也一樣。所以一個槽位可以先由 `antigravity` 或 `codex` 回答，Claude Code 看到的仍是 Claude id。群組的說明見 [Groups](/zh-TW/app/groups)。

CLI 也會開啟 gateway model discovery，群組裡每個名稱都會出現在 `/model` 選單。它不會寫 `ANTHROPIC_MODEL`，所以你用 `/model` 存的預設會保留。
:::

要還原設定，或查看 CLI 管理了哪些項目，見[設定 agent](/zh-TW/cli/agents)。群組會留在 **Groups** 頁面，隨時可以調整順序或加入備援目標。

只想在這一次 session 使用 proxy 的話，改用 CLI 啟動 Claude Code：

```console
$ kano-proxy agent launch claude-code
$ kano-proxy agent launch claude-code -- --model <name>    # -- 後面的參數都交給 claude
```

它不會寫入 `settings.json`。它直接沿用現有的 `claude-code` 群組，建立一把只給這次 session 用的 key，Claude Code 結束時就撤銷這把 key。它的設定優先於你自己的 `settings.json`，所以就算那個檔案指向別的 gateway 也能用。見[設定 agent](/zh-TW/cli/agents#單次啟動-agent)。

## 手動設定

1. 在 **Access** 頁面建立一把 key。
2. 把下面這段加進 `~/.claude/settings.json`。`ANTHROPIC_DEFAULT_*_MODEL` 請換成你 **Models** 頁面上的 id，這裡的只是範例。

```json
{
  "disableClaudeAiConnectors": true,
  "env": {
    "ANTHROPIC_BASE_URL": "https://<your-domain>/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<your-api-key>",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-code/claude-sonnet-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-code/claude-haiku-4-5",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "claude-code/claude-fable-5-1"
  }
}
```

3. 啟動 `claude`。

不要把 key 放進專案裡會被 commit 的 `.claude/settings.json`。

::: details 每個設定的作用
- `env` 區塊裡的值套用到每次啟動，而且優先於 shell 的環境變數。
- `ANTHROPIC_BASE_URL` 是 gateway 根路徑，**不含** `/v1`。Claude Code 會自己接上 `/v1/messages`。
- `ANTHROPIC_AUTH_TOKEN` 以 `Authorization: Bearer` 送出。`ANTHROPIC_API_KEY` 也可以，會以 `x-api-key` 送出，proxy 兩種都接受。擇一即可。
- `ANTHROPIC_DEFAULT_*_MODEL` 決定 `/model` 選單裡 Fable、Opus、Sonnet、Haiku 各對應到哪個 id。Haiku 也是 Claude Code 跑背景工作的模型。
- `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 讓 Claude Code 啟動時向 proxy 讀取 `GET /v1/models`，把結果加進 `/model` 選單。
- `disableClaudeAiConnectors: true` 放在頂層，和 `env` 同一層。它會關掉啟動時「claude.ai connectors 因為有其他驗證來源而停用」的提示。設了 proxy 的 token 之後，這些 connectors 本來就不會載入。
- 除非要固定啟動時的模型，否則不要設 `ANTHROPIC_MODEL`。它每次啟動都會蓋掉你用 `/model` 存的預設。
:::

只想試一次的話，用[上面 CLI 一節](#用-cli-設定)的 `kano-proxy agent launch claude-code`。不用 CLI 的話，也可以在 shell 匯出同一組變數。因為 `env` 區塊優先於 shell，只有 `settings.json` 沒設同樣的變數時，匯出的值才會生效：

::: code-group

```bash [macOS / Linux]
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

```powershell [Windows]
$env:ANTHROPIC_BASE_URL = "https://<your-domain>/anthropic"
$env:ANTHROPIC_AUTH_TOKEN = "<your-api-key>"
$env:ANTHROPIC_MODEL = "claude-code/claude-opus-5"
claude
```

:::

## 切換模型

在 session 裡用 `/model <id>` 可以切到 proxy 上任何 id，例如 `/model codex/gpt-5.6-sol`。按 Enter 存成預設，按 `s` 只在這次 session 有效。

Gateway model discovery 只保留含 `claude` 或 `anthropic` 的 id。Claude 的 id 會出現在選單裡，`codex/...` 不會，要自己輸入。模型 id 的規則見[端點與模型 id](/zh-TW/reference/endpoints)。

## 確認可用

1. 在 Claude Code 裡執行 `/status`，會顯示 base URL 以及是否使用 auth token。
2. 送出一則訊息。幾秒內 [Logs](/zh-TW/app/logs) 頁面就會出現這筆請求。

## 限制

- Claude Code 會直接向 `api.anthropic.com` 檢查 fast mode 能不能用，不走 `ANTHROPIC_BASE_URL`。如果這台機器從沒檢查成功過，只設了 `ANTHROPIC_AUTH_TOKEN`（也就是上面的設定），也沒有登入 claude.ai，它就不會送出這個檢查，`/fast` 會顯示 `Fast mode has been disabled by your organization`。設定 `CLAUDE_CODE_SKIP_FAST_MODE_ORG_CHECK=1` 就能使用。改用 `ANTHROPIC_API_KEY` 時，`/fast` 會說 fast mode 無法使用，這時要設的是 `CLAUDE_CODE_SKIP_FAST_MODE_NETWORK_ERRORS=1`。如果用 token 時也登入了 claude.ai，或這台機器留有之前檢查成功的結果，不設這些變數 `/fast` 也可能可以用。fast mode 的請求會和其他請求一樣經過 proxy。Claude 訂閱的 fast mode 會用額外用量點數計費。要讓某個帳號維持標準速度，在 [Providers](/zh-TW/app/providers#轉送前的修改) 勾選它的 **Remove fast mode**。
- WebFetch 工具的網域安全檢查會直接連 `api.anthropic.com`。如果你的網路擋了這個網域，在設定裡加上 `skipWebFetchPreflight: true`。
- Claude 桌面版 app 不讀這些變數。它有自己的第三方推論設定表單，把同一組 base URL 和 key 填進去即可。
- VS Code 擴充套件從 `claudeCode.environmentVariables` 設定讀取這些變數，不讀 shell。
- 用 `kano-proxy agent launch` 啟動的 session 不能移到背景（`/bg`、`--bg`），因為指令結束時它的 key 就被撤銷了。

## 查證來源

Claude Code 文件，2026-09-04：[Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)、[Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol)、[Model configuration](https://code.claude.com/docs/en/model-config)。2026-11-08，fast mode 與 WebFetch 檢查：[Fast mode](https://code.claude.com/docs/en/fast-mode)、[Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)。2026-11-13 以 Claude Code 2.1.322 驗證 `kano-proxy agent launch`，以及 `env` 區塊優先於 shell：在 `settings.json` 指向別的 gateway 的情況下，實際送出的是 launch 的端點、key 和模型。
