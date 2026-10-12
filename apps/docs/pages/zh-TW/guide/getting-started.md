---
title: 開始使用
description: 登入、連接供應商、用 kano-proxy CLI 設定 coding agent 或用 curl 送出請求，再到 Logs 頁面確認。
---

# 開始使用

登入、連接一個供應商、把一個工具指向 proxy，最後確認請求有送到。幾分鐘就能完成。

## 1. 登入

打開 `https://<your-domain>/login`，選 **Continue with Google**。沒有密碼。你看到的供應商、金鑰和紀錄都屬於你自己的帳號。

## 2. 連接供應商

1. 從側邊欄打開 **Providers**。
2. 選一個供應商，例如 Claude Code，再按 **Add account**。
3. 在跳出的視窗登入該供應商。這是供應商自己的登入流程，用的是你已經付費的訂閱。

完成後，帳號會出現在 **Providers**，並顯示用量。同一個供應商可以加好幾個帳號，一個用完，請求就會換到下一個。自訂端點和本機模型也在同一頁連接，詳見 [Providers](/zh-TW/app/providers)。

## 3. 設定 coding agent

最快的方法是用 `kano-proxy` CLI。它會幫你建立金鑰、寫好工具的設定檔。目前支援 Claude Code 和 Codex CLI。

1. 安裝 CLI，見[安裝與登入](/zh-TW/cli/install)。
2. 讓這台電腦登入。CLI 會在瀏覽器打開一個頁面，在那裡核准這台裝置，再把頁面顯示的一次性代碼貼回終端機。

   ```bash
   kano-proxy login
   ```

3. 設定工具。Codex CLI 請把 `claude-code` 換成 `codex`。

   ```bash
   kano-proxy agent setup claude-code
   ```

4. 照平常的方式啟動工具。

CLI 會問你工具的每個模型欄位要由哪些模型回應。這些問題的意思和還原方法見[設定 agent](/zh-TW/cli/agents)。Cursor、Cline、OpenCode 等其他工具，請照 **Coding agent 設定**底下各自的頁面手動設定。

## 或直接呼叫 API

1. 打開 **Access**，按 **Create key**。金鑰出現時就複製下來。它以 `sk-kano-proxy-` 開頭，只會顯示這一次。
2. 到 **Models** 挑一個模型 id，格式是 `provider/model`。
3. 送出請求。下面的模型 id 只是範例，請換成你清單裡的 id。

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "messages": [{"role": "user", "content": "用五個字打招呼。"}],
    "max_tokens": 32
  }'
```

同一把金鑰、同一個模型 id 在 Anthropic 端點也能用：

```bash
curl https://<your-domain>/anthropic/v1/messages \
  -H "x-api-key: <your-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "用五個字打招呼。"}]
  }'
```

完整的網址、標頭與 id 規則見[端點與模型 id](/zh-TW/reference/endpoints)。

## 4. 確認有成功

打開 **Logs**。幾秒內你的請求就會出現在最上面，列出模型、回應的帳號、送出的金鑰、狀態、用掉的 token 和估算花費。如果請求失敗，看狀態碼再對照[錯誤訊息](/zh-TW/reference/errors)就知道原因。

## 接下來

- [介面說明](/zh-TW/app/overview)：每一頁顯示什麼、能做什麼。
- [請求如何分配](/zh-TW/reference/routing)：proxy 怎麼挑帳號，帳號用完時會發生什麼。
- [Groups](/zh-TW/app/groups)：替工具取自己的模型名稱，或跨供應商備援。
