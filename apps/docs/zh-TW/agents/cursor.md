---
title: Cursor
description: 透過 Cursor 的 OpenAI API Key 與 Override OpenAI Base URL 設定使用 Kano Proxy 的模型，以及哪些 Cursor 功能不會走這條路。
---

# Cursor

聊天與 Agent 可透過 Cursor 的 OpenAI 相容路徑使用。Tab 補全、subagent 與 Cursor 內建模型不會走你的金鑰。開啟前先看限制。

## 設定

在 **Cursor Settings › Models**：

1. 開啟 **OpenAI API Key**，貼上 `<your-api-key>`。
2. 開啟 **Override OpenAI Base URL**，填入 `https://<your-domain>/openai/v1`。
3. 用 **+ Add model** 輸入你 Models 頁面上的模型 id，例如 `claude-code/claude-opus-5` 或 `codex/gpt-5.4`。
4. 在聊天的模型選單選那個模型。

Claude 模型同樣走 OpenAI 相容端點。Cursor 的 Anthropic 金鑰沒有 base URL 覆寫，該欄位請保持關閉。

## 限制

- **覆寫是全域的。** 開啟期間，所有使用你自己金鑰的模型都會被導向 proxy。Cursor 內建模型與 Anthropic 金鑰的模型會失效。想用回它們就先關掉覆寫。Cursor 官方人員將此列為已知問題。
- **Tab** 永遠用 Cursor 自己的模型。
- **Subagent** 不理會自訂金鑰，計費算在你的 Cursor 方案。
- **背景與雲端 agent** 沒有 base URL 覆寫。
- 請求會經過 Cursor 的伺服器再到 proxy，所以 proxy 必須能從網際網路連到，且你的 `sk-kano-proxy-` 金鑰每次請求都會經過 Cursor。請用專屬金鑰，方便單獨撤銷。

## 查證來源

Cursor 文件與官方人員在論壇的回覆，2026-09-04：[API keys](https://cursor.com/docs/settings/api-keys)、[Override OpenAI Base URL](https://forum.cursor.com/t/override-openai-base-url-not-working/168323)、[開啟覆寫後 Anthropic 模型失效](https://forum.cursor.com/t/anthropic-models-break-when-override-openai-baseurl-is-set/144899)、[Subagent 忽略覆寫](https://forum.cursor.com/t/custom-models-openai-base-url-override-not-usable-in-subagents-silently-ignored/159369)。官方文件頁沒有描述 base URL 覆寫，該部分依據官方人員的回覆。
