---
title: 本機模型（CLI）
description: 用 kano-proxy CLI 把 Ollama、LM Studio、vLLM 或任何本機的 OpenAI／Anthropic 相容伺服器變成 Kano Proxy 的供應商。
---

# 用 CLI 接本機模型

`kano-proxy` CLI 跟你電腦上的模型伺服器跑在一起，主動**向外**連到 proxy。你的機器不會暴露在網路上：不用轉 port、不用 tunnel 服務、沒有公開網址。proxy 收到該供應商的請求後，會從這條連線送回來。

任何在 localhost 上講 OpenAI 或 Anthropic API 的伺服器都可以：Ollama、LM Studio、vLLM、llama.cpp server 等。

## 安裝

擇一：

```bash
brew install yufeng-kano/tap/kano-proxy
```

```bash
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
```

```powershell
scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket
scoop install kano-proxy
```

之後用 `kano-proxy update` 原地升級。透過 Homebrew 或 Scoop 安裝的，改用該管理工具升級。

## 登入這台裝置

```console
$ kano-proxy init
  Server: https://<your-domain>
  Device name [my-mac]:
```

CLI 會在瀏覽器打開登入頁。在那裡核准這台裝置，再把畫面上的代碼貼回終端機。每台機器只需做一次。無畫面的主機可以在任何瀏覽器打開印出的網址。

## 註冊本機伺服器

```console
$ kano-proxy tunnel add
  Slug [my-mac]:
  Type: openai
  Target [http://localhost:11434/v1]:
```

- **Slug** 會成為供應商前綴，模型以 `my-mac/<model>` 呼叫。
- **Type** 是本機伺服器講的 API，`openai` 或 `anthropic`。
- **Target** 是本機的 base URL。`openai` 要包含 `/v1`（`http://localhost:11434/v1`）；`anthropic` 只填 origin（`http://localhost:11434`），因為 CLI 會自己接上 `/v1/messages`。

CLI 會向伺服器要模型清單。保留預設的「All models」，之後新拉的模型會自動出現。也可以只挑一部分公開。

## 啟動 tunnel

```console
$ kano-proxy tunnel start
  my-mac: connected, 3 models reported
```

讓它一直跑著。從任何地方的任何客戶端，本機模型現在都是 proxy 上普通的模型 id：

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "my-mac/llama3.3:70b", "messages": [{"role": "user", "content": "hi"}]}'
```

也能當作[模型群組](/zh-TW/guide/endpoints#模型群組)的目標，例如排在雲端模型後面當備援。

## 管理裝置

- `kano-proxy tunnel list` 與 `kano-proxy status` 顯示已註冊與連線中的項目。
- `kano-proxy tunnel remove <slug>` 刪除一個供應商。
- `kano-proxy logout` 讓這台機器登出：還原 CLI 在這裡設定過的 coding agent、撤銷裝置與該裝置建立的 API key，並清掉本機狀態。
- 應用程式的 **Access** 頁面列出所有裝置，可以撤銷；撤銷裝置會一併撤銷它建立的 key。撤銷的裝置會在一小時內斷線，且無法重連。**Providers** 頁面列出已註冊的本機端點。

同一支 CLI 也能把 [Claude Code](/zh-TW/agents/claude-code) 與 [Codex CLI](/zh-TW/agents/codex-cli) 接到你的 proxy：`kano-proxy agent setup claude-code`、`kano-proxy agent setup codex`。

CLI 雙向串流不緩衝、網路斷線自動重連、絕不記錄請求內容。
