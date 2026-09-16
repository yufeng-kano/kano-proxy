---
title: Cline
description: 設定 Cline 的 OpenAI Compatible 供應商使用 Kano Proxy，以及為什麼不該選 Anthropic 供應商。
---

# Cline

用 **OpenAI Compatible** 供應商即可。Claude 模型也走這條：Cline 的 Anthropic 供應商模型清單是固定的，無法輸入 `provider/model` id。

## 設定

在 Cline 設定裡選 **API Provider: OpenAI Compatible**，填入：

| 欄位 | 值 |
|------|----|
| Base URL | `https://<your-domain>/openai/v1` |
| API Key | `<your-api-key>` |
| Model ID | 你 Models 頁面上的 id，例如 `claude-code/claude-opus-5` |

要包含 `/v1`。Cline 會原樣使用 base URL，再接上 `/chat/completions`。

Cline 也會向 proxy 讀 `GET /openai/v1/models`，所以你的 id 會出現在它的模型下拉選單。若沒出現，選 **Use custom model ID** 手動輸入。

## Anthropic 供應商

Cline 的 Anthropic 供應商有 **Use custom base URL** 選項。它送到 `<base>/messages`，所以值必須是 `https://<your-domain>/anthropic/v1`。但它的模型選單只有 Cline 內建的 Claude 名稱，無法輸入 `claude-code/claude-opus-5`，而 proxy 會拒絕沒有前綴的名稱。請留在 OpenAI Compatible。

## 查證來源

Cline 文件與原始碼，2026-09-04：[OpenAI Compatible](https://docs.cline.bot/provider-config/openai-compatible)、[Anthropic](https://docs.cline.bot/provider-config/anthropic)，以及 [cline 儲存庫](https://github.com/cline/cline)的供應商程式碼。`/v1` 的要求與缺少自訂模型欄位這兩點是從原始碼確認的，文件沒有寫。
