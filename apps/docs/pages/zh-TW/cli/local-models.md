---
title: 本機模型
description: 用 kano-proxy tunnel 把 Ollama、LM Studio、vLLM、llama.cpp 或任何本機的 OpenAI、Anthropic 相容伺服器變成 Kano Proxy 的供應商，不需要公開網址。
---

# 本機模型

`kano-proxy tunnel` 能把你自己機器上的模型伺服器變成 proxy 上的供應商。CLI 會**主動向外**連到 proxy，送給這個供應商的請求再沿著同一條連線傳回來。你的機器不會對外開放任何東西：不用設定 port forwarding，不用 tunnel 服務，也不需要公開網址。

只要是在本機提供 OpenAI 或 Anthropic API 的伺服器都能用，例如 Ollama、LM Studio、vLLM、llama.cpp server。

機器要先登入（[安裝與登入](/zh-TW/cli/install)）。

## 1. 註冊本機伺服器

```bash
kano-proxy tunnel add
```

畫面會列出五個設定和它們的預設值。按 Enter 打開一個項目修改，在清單按 **Tab** 就會完成註冊。按鍵用法跟 [agent setup](/zh-TW/cli/agents#選擇模型) 相同。

- **Slug**：這個供應商在 proxy 上的名稱，模型會叫做 `<slug>/<model>`。預設是這台機器的主機名稱，例如 `my-mac`。之後無法更改。
- **API**：本機伺服器提供的 API，`openai` 或 `anthropic`。
- **Target**：本機伺服器的 base URL。畫面會依你選的 API 顯示正確格式和範例，其餘路徑由 CLI 自動補上。
- **Local API key**：只有本機伺服器需要 key 時才填，大多數都不需要。它只存在這台機器上，proxy 不會看到。
- **Models**：要開放哪些模型（見下方）。

常見的 Target：

| 伺服器 | `openai` | `anthropic` |
|--------|----------|-------------|
| Ollama | `http://localhost:11434/v1` | `http://localhost:11434` |
| LM Studio | `http://localhost:1234/v1` | |
| vLLM | `http://localhost:8000/v1` | |
| llama.cpp server | `http://localhost:8080/v1` | `http://localhost:8080` |

`openai` 的 Target 要包含 `/v1`；`anthropic` 只填到 port 為止。

**Models** 項目會向本機伺服器查詢模型清單。保留第一個選項 **All models (follow local server)**，之後在本機新增的模型就會自動出現在 proxy 上。也可以只挑部分模型開放。如果本機伺服器還沒啟動，可以先手動輸入模型 id，第一次連線後會被實際清單取代。

可以註冊多個本機伺服器，每一個都是獨立的供應商。

::: details 不用互動畫面
```bash
kano-proxy tunnel add --no-tui --slug my-mac --format openai --target http://localhost:11434/v1
```

`--expose m1,m2` 限制開放的模型；不加就開放全部。本機 API key 請用環境變數 `KANO_PROXY_TARGET_KEY` 傳入，不要用 `--target-key`，否則會留在 shell 歷史紀錄裡。
:::

## 2. 啟動 tunnel

```bash
kano-proxy tunnel start
```

它會為每個註冊的本機伺服器各開一條連線，每次連線和每個請求各印一行。它在前景執行，讓它一直開著就好，按 Ctrl-C 停止。想在背景常駐的話，請自己用 launchd 或 systemd 的服務來跑，CLI 不會自己建立服務。

執行期間，本機模型就是 proxy 上的一般模型 id，任何地方的任何客戶端都能呼叫：

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "my-mac/llama3.3:70b", "messages": [{"role": "user", "content": "hi"}]}'
```

`my-mac/llama3.3:70b` 只是範例，請換成你 Models 頁上的 slug 和模型。本機模型也能當 [model group](/zh-TW/app/groups) 的目標，例如放在雲端模型後面當備援。

## 運作方式

- **本機新增的模型會自動出現。** 連線期間，CLI 每 5 分鐘檢查一次本機的模型清單，有變動就回報。
- **網路中斷會自動重連。** 重試間隔會逐漸拉長，最長一分鐘。
- **筆電闔上也不影響 failover。** 機器離線時，請求會暫時略過這個供應商 60 秒，改走 group 裡的下一個目標。重新連上後立刻恢復可用。
- **每個本機伺服器同時最多 4 個請求。** 超過的請求會立刻被拒絕，讓 group 直接改試下一個目標，而不是排隊等待。`--concurrency` 設為 1 到 3 可以調低上限。
- **串流會即時轉送**，取消的請求也會在本機伺服器上一併取消。
- **只記錄請求本身的摘要**：method、路徑、狀態碼和耗時。絕不記錄內容或 header。

proxy 和 CLI 只會轉送到你註冊的 Target，而且只限模型 API 的路徑。tunnel 無法存取你機器上的其他任何東西。

## 管理本機伺服器

- `kano-proxy tunnel list`：列出這台機器的本機伺服器，包含 slug、API、Target、連線狀態、模型數量，以及最後一次回報的時間。
- `kano-proxy tunnel remove <slug>`：從 proxy 和這台機器上移除。加 `--local-only` 只在這台機器上移除，用於已經在網頁上刪掉的情況。
- `kano-proxy tunnel attach <slug> --target <url>`：讓這台機器接手另一台機器註冊的供應商，例如換電腦之後。

[Providers](/zh-TW/app/providers) 頁的 **CLI providers** 區塊會列出所有本機伺服器，包含狀態、模型數量和來源裝置，也可以在那裡重新命名或移除。本機模型跟其他供應商一樣，會出現在 [Models](/zh-TW/app/models)、[Logs](/zh-TW/app/logs) 和 group 的目標選單裡。

機器登出請見 [登出](/zh-TW/cli/install#登出)。登出後它的本機伺服器仍會保持註冊，顯示為離線，直到你移除為止。

## 相關頁面

- [Providers](/zh-TW/app/providers)
- [Groups](/zh-TW/app/groups)
- [請求如何分配](/zh-TW/reference/routing)
