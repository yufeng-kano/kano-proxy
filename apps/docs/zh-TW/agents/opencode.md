---
title: OpenCode
description: 在 opencode.json 用 @ai-sdk/openai-compatible 把 Kano Proxy 加成自訂供應商，API key 從環境變數讀取。
---

# OpenCode

完整可用。OpenCode 可以在設定檔裡把任何 OpenAI 相容端點加成自訂供應商。

## 設定

全域設定放在 `~/.config/opencode/opencode.json`，單一專案則放在專案根目錄的 `opencode.json`：

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
        "codex/gpt-5.4": { "name": "GPT-5.4" }
      }
    }
  },
  "model": "kano/claude-code/claude-opus-5"
}
```

然後匯出金鑰：

```bash
export KANO_PROXY_API_KEY=<your-api-key>
```

- `kano` 是你給供應商取的名字。模型參照格式是 `<供應商名>/<模型鍵>`，所以 `claude-code/claude-opus-5` 裡的斜線不會有問題。
- `models` 底下列出你想出現在選單的 id，取自你的 Models 頁面。顯示用的 `name` 可以自由填寫。
- `{env:VAR}` 在啟動時讀環境變數。變數不存在時會變成空字串且不報錯，所以驗證失敗通常表示忘了匯出。

## 改用 Anthropic 格式

OpenCode 也可以載入 `@ai-sdk/anthropic` 走 Anthropic 端點。這個 SDK 會接上 `/messages`，所以 base 必須以 `/v1` 結尾：

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

名字不要用 `anthropic`，覆寫內建的 `anthropic` 供應商會讓 OpenCode 加上 Anthropic 專用的 beta 標頭。OpenCode 文件只描述 OpenAI 相容的自訂供應商，Anthropic 這種寫法是從原始碼與第三方 gateway 指南確認的，文件沒有寫。

## 查證來源

OpenCode 文件與原始碼，2026-09-04：[Providers](https://opencode.ai/docs/providers/)、[Config](https://opencode.ai/docs/config/)、[Models](https://opencode.ai/docs/models/)，以及 [opencode 儲存庫](https://github.com/anomalyco/opencode)的供應商載入程式。
