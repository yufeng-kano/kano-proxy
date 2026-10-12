---
title: OpenCode
description: 在 opencode.json 用 @ai-sdk/openai-compatible 把 Kano Proxy 加成自訂供應商，API key 從環境變數讀取。
---

# OpenCode

完整可用。OpenCode 可以在設定檔裡把任何 OpenAI 相容端點加成自訂供應商。

kano-proxy CLI 不支援設定 OpenCode，請手動設定。

## 手動設定

1. 在 **Access** 頁面建立一把 key。
2. 所有專案共用的設定放在 `~/.config/opencode/opencode.json`，只給單一專案用就放在專案根目錄的 `opencode.json`。模型 id 只是範例，請列出你 **Models** 頁面上的 id。

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "kano": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Kano Proxy",
      "options": {
        "baseURL": "https://<your-domain>/openai/v1",
        "apiKey": "{env:KANO_PROXY_API_KEY}"
      },
      "models": {
        "claude-code/claude-opus-5": { "name": "Claude Opus 5" },
        "codex/gpt-5.6-sol": { "name": "GPT-5.6-Sol" }
      }
    }
  },
  "model": "kano/claude-code/claude-opus-5"
}
```

3. 在要啟動 OpenCode 的 shell 匯出 key：

::: code-group

```bash [macOS / Linux]
export KANO_PROXY_API_KEY=<your-api-key>
```

```powershell [Windows]
$env:KANO_PROXY_API_KEY = "<your-api-key>"
```

:::

4. 啟動 `opencode`。

::: details 每個設定的作用
- `kano` 是你替這個供應商取的名字。模型參照的格式是 `<供應商名稱>/<模型鍵>`，所以 `claude-code/claude-opus-5` 裡的斜線沒有問題。
- `models` 列出你想放進選單的 id，顯示用的 `name` 可以自由填。模型 id 的規則見[端點與模型 id](/zh-TW/reference/endpoints)。
- `{env:VAR}` 在啟動時讀取環境變數。變數不存在時會變成空字串，而且不會報錯，所以驗證失敗通常是忘了匯出。
:::

### 改用 Anthropic 格式

OpenCode 也能載入 `@ai-sdk/anthropic`，改走 Anthropic 端點。這個 SDK 會自己接上 `/messages`，所以 base 必須以 `/v1` 結尾：

```json
{
  "provider": {
    "kano-anthropic": {
      "npm": "@ai-sdk/anthropic",
      "name": "Kano Proxy (Anthropic)",
      "options": {
        "baseURL": "https://<your-domain>/anthropic/v1",
        "apiKey": "{env:KANO_PROXY_API_KEY}"
      },
      "models": {
        "claude-code/claude-opus-5": { "name": "Claude Opus 5" }
      }
    }
  }
}
```

供應商名稱不要用 `anthropic`。

::: details 為什麼不能叫 `anthropic`
覆寫內建的 `anthropic` 供應商時，OpenCode 會加上 Anthropic 專用的 beta 標頭。OpenCode 文件只描述 OpenAI 相容的自訂供應商，Anthropic 這種寫法是從原始碼和第三方 gateway 指南確認的，文件沒有寫。
:::

## 確認可用

用清單裡的模型在 OpenCode 送出一則訊息。幾秒內 [Logs](/zh-TW/app/logs) 頁面就會出現這筆請求。如果出現驗證錯誤，先確認那個 shell 有匯出 `KANO_PROXY_API_KEY`。

## 限制

透過 proxy 使用時，目前沒有已知的限制。

## 查證來源

OpenCode 文件與原始碼，2026-09-04：[Providers](https://opencode.ai/docs/providers/)、[Config](https://opencode.ai/docs/config/)、[Models](https://opencode.ai/docs/models/)，以及 [opencode 儲存庫](https://github.com/anomalyco/opencode)的供應商載入程式。
