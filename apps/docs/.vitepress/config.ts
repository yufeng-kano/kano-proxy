import { defineConfig, type DefaultTheme } from "vitepress"

/**
 * Public documentation site, served at /docs/ from the same Pages project as
 * the admin UI (docs/docs-site.md). Static only: no session, no /api calls.
 *
 * Hostnames never live in tracked files, so the sitemap hostname comes from
 * APP_URL at build time (release CI sets it); unset means no sitemap.
 */
const appUrl = process.env.APP_URL?.replace(/\/+$/, "")

const SITE_NAME = "Kano Proxy"
const DESCRIPTION_EN =
  "Connect the AI subscriptions you already pay for, then point any OpenAI- or Anthropic-compatible client at a single URL."
const DESCRIPTION_ZH =
  "接上你已付費的 AI 訂閱，讓任何 OpenAI 或 Anthropic 相容的客戶端都指向同一個網址。"

function sidebarEn(): DefaultTheme.SidebarItem[] {
  return [
    {
      text: "Guide",
      items: [
        { text: "Getting started", link: "/guide/getting-started" },
        { text: "Endpoints and model ids", link: "/guide/endpoints" },
        { text: "Local models (CLI)", link: "/guide/local-models" },
      ],
    },
    {
      text: "Coding agents",
      items: [
        { text: "Claude Code", link: "/agents/claude-code" },
        { text: "Codex CLI", link: "/agents/codex-cli" },
        { text: "Cursor", link: "/agents/cursor" },
        { text: "Cline", link: "/agents/cline" },
        { text: "OpenCode", link: "/agents/opencode" },
        { text: "Gemini CLI", link: "/agents/gemini-cli" },
      ],
    },
  ]
}

function sidebarZh(): DefaultTheme.SidebarItem[] {
  return [
    {
      text: "指南",
      items: [
        { text: "開始使用", link: "/zh-TW/guide/getting-started" },
        { text: "端點與模型 id", link: "/zh-TW/guide/endpoints" },
        { text: "本機模型（CLI）", link: "/zh-TW/guide/local-models" },
      ],
    },
    {
      text: "Coding agent 設定",
      items: [
        { text: "Claude Code", link: "/zh-TW/agents/claude-code" },
        { text: "Codex CLI", link: "/zh-TW/agents/codex-cli" },
        { text: "Cursor", link: "/zh-TW/agents/cursor" },
        { text: "Cline", link: "/zh-TW/agents/cline" },
        { text: "OpenCode", link: "/zh-TW/agents/opencode" },
        { text: "Gemini CLI", link: "/zh-TW/agents/gemini-cli" },
      ],
    },
  ]
}

export default defineConfig({
  base: "/docs/",
  title: SITE_NAME,
  cleanUrls: true,
  lastUpdated: true,
  // Same mark as the admin UI's favicon (apps/web/index.html). Not
  // base-prefixed automatically, so the path is written in full.
  head: [
    [
      "link",
      {
        rel: "icon",
        href: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' rx='6' fill='%23111111'/%3E%3Ctext x='16' y='22' text-anchor='middle' font-size='16' font-family='system-ui' fill='%23f5f5f5'%3Ek%3C/text%3E%3C/svg%3E",
      },
    ],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:site_name", content: SITE_NAME }],
    ["meta", { name: "twitter:card", content: "summary" }],
  ],
  // VitePress emits <title> and <meta name="description"> per page from the
  // frontmatter; these two mirror them for link previews.
  transformPageData(pageData) {
    const title =
      pageData.title && pageData.title !== SITE_NAME ? `${pageData.title} | ${SITE_NAME}` : SITE_NAME
    const description = pageData.description || pageData.frontmatter.description || ""
    pageData.frontmatter.head ??= []
    pageData.frontmatter.head.push(
      ["meta", { property: "og:title", content: title }],
      ["meta", { property: "og:description", content: description }],
    )
  },
  sitemap: appUrl ? { hostname: `${appUrl}/docs/` } : undefined,
  locales: {
    root: {
      label: "English",
      lang: "en",
      description: DESCRIPTION_EN,
      themeConfig: {
        nav: [
          { text: "Guide", link: "/guide/getting-started", activeMatch: "/guide/" },
          { text: "Coding agents", link: "/agents/claude-code", activeMatch: "/agents/" },
        ],
        sidebar: sidebarEn(),
        outline: { label: "On this page" },
      },
    },
    "zh-TW": {
      label: "繁體中文",
      lang: "zh-TW",
      link: "/zh-TW/",
      description: DESCRIPTION_ZH,
      themeConfig: {
        nav: [
          { text: "指南", link: "/zh-TW/guide/getting-started", activeMatch: "/zh-TW/guide/" },
          { text: "Coding agent", link: "/zh-TW/agents/claude-code", activeMatch: "/zh-TW/agents/" },
        ],
        sidebar: sidebarZh(),
        outline: { label: "本頁內容" },
        docFooter: { prev: "上一頁", next: "下一頁" },
        lastUpdated: { text: "最後更新" },
        langMenuLabel: "切換語言",
        returnToTopLabel: "回到頂端",
        sidebarMenuLabel: "選單",
        darkModeSwitchLabel: "外觀",
        lightModeSwitchTitle: "切換為淺色",
        darkModeSwitchTitle: "切換為深色",
      },
    },
  },
  themeConfig: {
    // The language menu keeps the reader on the same topic (/guide/x ↔
    // /zh-TW/guide/x). Stated explicitly because the docs promise it.
    i18nRouting: true,
    search: {
      provider: "local",
      options: {
        locales: {
          "zh-TW": {
            translations: {
              button: { buttonText: "搜尋", buttonAriaLabel: "搜尋" },
              modal: {
                displayDetails: "顯示詳細內容",
                resetButtonTitle: "清除",
                backButtonTitle: "關閉",
                noResultsText: "沒有結果",
                footer: {
                  selectText: "開啟",
                  selectKeyAriaLabel: "enter",
                  navigateText: "切換",
                  navigateUpKeyAriaLabel: "上",
                  navigateDownKeyAriaLabel: "下",
                  closeText: "關閉",
                  closeKeyAriaLabel: "esc",
                },
              },
            },
          },
        },
      },
    },
  },
})
