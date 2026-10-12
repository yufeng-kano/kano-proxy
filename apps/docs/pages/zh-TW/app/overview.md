---
title: Overview
description: Kano Proxy 的 Overview 頁面：一天、一週或一個月內的估計花費、請求數、token、快取與各模型用量，以及怎麼看。
---

# Overview

Overview 顯示你在某一天、某一週或某個月用了多少：估計花費、請求數、token、快取，以及是哪些模型在做事。登入後第一個看到的就是這頁。

## 頁面上有什麼

### 時間範圍

頁首的 **Day**、**Week**、**Month** 決定下面所有數字的範圍。

- **Day**：一天，每小時一根長條。
- **Week**：週一到週日，每天一根長條。
- **Month**：一個月，每天一根長條。

再按一次目前選中的那一項，會打開日曆讓你挑日期、週或月份。時間以瀏覽器的時區為準。旁邊的重新整理按鈕會重新載入數字。

### 三張指標卡

最上面是 **Spend**、**Requests**、**Tokens** 三張卡，每張都有：

- 這段期間的總數；
- 一張小的堆疊長條圖，每個模型一種顏色；
- 依這個指標排序的前四名模型，其餘合併成 **Others**。

**Spend** 是以美元計的估計值，不是帳單。它用每個模型公開的 API 牌價乘上 token 數，透過訂閱帳號送出的請求也照算。總數下方那行說明估計涵蓋多少：

- **Estimated from list prices**：期間內每個請求都有價格。
- **Estimated · covers N of M requests**：有些模型查不到價格，所以總數偏低。
- **No priced requests in this range**：完全沒有可計價的請求，總數顯示 `—`，而不是 `$0.00`。

**Tokens** 是輸入加輸出的 token 數。

按卡片上的展開按鈕，會打開較大的圖表，並附一張表，列出每個模型在每根長條的 **Min**、**Max**、**Avg**、**Sum**。**Avg** 不計該模型沒有流量的長條。

### Activity

下面這張卡有四個分頁。卡片標題列另外顯示 **Errors**（失敗的請求數）和 **Avg latency**（[Logs](/zh-TW/app/logs) 裡 **Latency** 欄的平均）。

| 分頁 | 內容 |
|------|------|
| **Tokens** | 每根長條的 token 數，依模型堆疊 |
| **Requests** | 每根長條的請求數，依模型堆疊 |
| **Caching** | 每根長條的輸入 token，分成 **Cached** 和 **Uncached** |
| **By model** | 每個模型一列：**Requests**、**Errors**、**Input**、**Cached**、**Cache write**、**Output**、**Cache rate**、**Spend** |

模型一律以完整的 `provider/model` id 顯示。透過 model group 送出的請求，算在實際回應的那個模型底下。同一個供應商的多個帳號會加總在同一列。想知道某一筆請求是哪個帳號、哪個 group 處理的，請看 [Logs](/zh-TW/app/logs)。

有些供應商不是每次都回報 token 數。這類請求仍會算進 **Requests** 和 **Errors**，但不算進 token 總數。這時 **Cache rate** 旁邊會註明涵蓋幾筆，例如 `on 40 of 52`。

已經移除的供應商，它的用量不會出現在這頁，Logs 裡仍查得到那些請求。

## 常見操作

### 看某一天花了多少

1. 選 **Day**。
2. 再按一次 **Day** 打開日曆。
3. 點選日期。
4. 看 **Spend**，展開後可以看到每個模型各花多少。

### 確認 prompt 快取有沒有作用

1. 選一段有流量的期間。
2. 打開 **Caching** 分頁。**Cached** 佔比高，代表重複的 prompt 是從快取讀的。
3. 打開 **By model**，比較各模型的 **Cache rate**。

### 找出用量暴增的原因

1. 在 **Tokens** 或 **Requests** 找到最高的那根長條，滑鼠移上去看是哪些模型。
2. 到 [Logs](/zh-TW/app/logs)，用該模型的供應商篩選，逐筆查看。

## 頁面是空的

出現 **No requests yet**，表示選定的期間內沒有任何請求用你的 key 打到 proxy。按 **Get your API key** 到 Access 頁建立一把。

## 相關頁面

- [Logs](/zh-TW/app/logs)：每一筆請求，含帳號、key 與錯誤。
- [Models](/zh-TW/app/models)：你可以呼叫的模型 id。
- [Access](/zh-TW/app/access)：API key 與每把 key 的花費上限。
