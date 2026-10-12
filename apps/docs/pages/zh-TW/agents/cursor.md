---
title: Cursor
description: 透過 Cursor 的 OpenAI API Key 與 Override OpenAI Base URL 設定使用 Kano Proxy 的模型，以及哪些 Cursor 功能不會走這條路。
---

# Cursor

聊天和 Agent 可以透過 Cursor 的 OpenAI 相容路徑使用；Tab 補全、subagent 和 Cursor 內建模型不會走你的 key，開啟前請先看限制。

kano-proxy CLI 不支援設定 Cursor，請手動設定。

## 手動設定

1. 在 **Access** 頁面建立一把 Cursor 專用的 key，之後可以單獨撤銷。
2. 在 **Cursor Settings › Models** 開啟 **OpenAI API Key**，貼上 `<your-api-key>`。
3. 開啟 **Override OpenAI Base URL**，填入 `https://<your-domain>/openai/v1`。
4. 用 **+ Add model** 輸入你 **Models** 頁面上的模型 id，例如 `claude-code/claude-opus-5` 或 `codex/gpt-5.6-sol`。模型 id 的規則見[端點與模型 id](/zh-TW/reference/endpoints)。
5. 在聊天的模型選單選這個模型。

Claude 模型也走 OpenAI 相容端點。Cursor 的 Anthropic key 請保持關閉。

::: details 為什麼不開 Anthropic key
Cursor 的 Anthropic key 沒有 base URL 覆寫。加在那裡的 Claude 模型會直接連到 Anthropic，不會經過 proxy。
:::

## 確認可用

用剛加入的模型送出一則聊天訊息。幾秒內 [Logs](/zh-TW/app/logs) 頁面就會出現這筆請求。

## 限制

- **覆寫是全域的。** 開啟期間，所有使用你自己 key 的模型都會被導向 proxy，Cursor 內建模型和用 Anthropic key 的模型都會失效。想用回它們，就先關掉覆寫。Cursor 官方人員已將此列為已知問題。
- **Tab** 永遠用 Cursor 自己的模型。
- **Subagent** 不理會自訂 key，費用算在你的 Cursor 方案。
- **背景與雲端 agent** 沒有 base URL 覆寫。
- **請求會經過 Cursor 的伺服器。** proxy 必須能從網際網路連到，而且每次請求你的 `sk-kano-proxy-` key 都會經過 Cursor。

## 查證來源

Cursor 文件與官方人員在論壇的回覆，2026-09-04：[API keys](https://cursor.com/docs/settings/api-keys)、[Override OpenAI Base URL](https://forum.cursor.com/t/override-openai-base-url-not-working/168323)、[開啟覆寫後 Anthropic 模型失效](https://forum.cursor.com/t/anthropic-models-break-when-override-openai-baseurl-is-set/144899)、[Subagent 忽略覆寫](https://forum.cursor.com/t/custom-models-openai-base-url-override-not-usable-in-subagents-silently-ignored/159369)。官方文件頁沒有描述 base URL 覆寫，這部分依據官方人員的回覆。
