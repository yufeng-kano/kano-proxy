import { defineConfig, type DefaultTheme, type UserConfig } from "vitepress"

/**
 * Public documentation site, served at /docs/ from the same web image as the
 * admin UI (docs/docs-site.md). Static only: no session, no /api calls.
 *
 * `config.ts` is the standalone site: `defineDocsConfig()` with nothing
 * added. An edition builds the same site from its own VitePress project,
 * passing the pages it adds (docs/docs-site.md § Editions).
 *
 * Hostnames never live in tracked files, so the sitemap hostname comes from
 * APP_URL at build time (release CI sets it); unset means no sitemap.
 */
const appUrl = process.env.APP_URL?.replace(/\/+$/, "")

/** What an edition adds to the site; every field is appended after the core's own. */
export interface DocsEditionOptions {
  /**
   * Pages for the edition's own app pages, appended to the core's "The app"
   * group so the sidebar keeps the app's order, per locale.
   */
  appPages?: {
    en?: DefaultTheme.SidebarItem[]
    "zh-TW"?: DefaultTheme.SidebarItem[]
  }
  /** Sidebar groups appended below the core's, per locale. */
  sidebar?: {
    en?: DefaultTheme.SidebarItem[]
    "zh-TW"?: DefaultTheme.SidebarItem[]
  }
  /** Top-nav entries appended after the core's, per locale. */
  nav?: { en?: DefaultTheme.NavItem[]; "zh-TW"?: DefaultTheme.NavItem[] }
  /**
   * VitePress dates a page from `git log`. An edition that builds from an
   * assembled copy of the pages has no history for them, so it turns this off
   * rather than dating every page to the build.
   */
  lastUpdated?: boolean
}

const SITE_NAME = "Kano Proxy"
const DESCRIPTION_EN =
  "Connect the AI subscriptions you already pay for, then point any OpenAI- or Anthropic-compatible client at a single URL."
const DESCRIPTION_ZH = "接上你已付費的 AI 訂閱，讓任何 OpenAI 或 Anthropic 相容的客戶端都指向同一個網址。"

type Labels = Record<
  | "start" | "gettingStarted" | "app" | "agents" | "cli" | "cliInstall" | "cliAgents"
  | "cliLocal" | "reference" | "endpoints" | "routing" | "errors",
  string
>

const LABELS: { en: Labels; "zh-TW": Labels } = {
  en: {
    start: "Get started",
    gettingStarted: "Getting started",
    app: "The app",
    agents: "Coding agents",
    cli: "CLI",
    cliInstall: "Install and sign in",
    cliAgents: "Set up agents",
    cliLocal: "Local models",
    reference: "Reference",
    endpoints: "Endpoints and model ids",
    routing: "How requests are routed",
    errors: "Errors",
  },
  "zh-TW": {
    start: "開始",
    gettingStarted: "開始使用",
    app: "介面說明",
    agents: "Coding agent 設定",
    cli: "CLI",
    cliInstall: "安裝與登入",
    cliAgents: "設定 agent",
    cliLocal: "本機模型",
    reference: "參考",
    endpoints: "端點與模型 id",
    routing: "請求如何分配",
    errors: "錯誤訊息",
  },
}

/**
 * The app's own sidebar labels (apps/web/src/i18n), which the "The app" pages
 * mirror. The app ships English only, so both locales show them as is.
 */
const APP_PAGES: [string, string][] = [
  ["Overview", "overview"],
  ["Logs", "logs"],
  ["Providers", "providers"],
  ["Models", "models"],
  ["Groups", "groups"],
  ["Access", "access"],
]

const AGENTS: [string, string][] = [
  ["Claude Code", "claude-code"],
  ["Codex CLI", "codex-cli"],
  ["Cursor", "cursor"],
  ["Cline", "cline"],
  ["OpenCode", "opencode"],
  ["Gemini CLI", "gemini-cli"],
]

function sidebar(locale: "en" | "zh-TW", appPages: DefaultTheme.SidebarItem[]): DefaultTheme.SidebarItem[] {
  const t = LABELS[locale]
  const p = locale === "en" ? "" : "/zh-TW"
  return [
    { text: t.start, items: [{ text: t.gettingStarted, link: `${p}/guide/getting-started` }] },
    {
      text: t.app,
      items: [...APP_PAGES.map(([text, slug]) => ({ text, link: `${p}/app/${slug}` })), ...appPages],
    },
    { text: t.agents, items: AGENTS.map(([text, slug]) => ({ text, link: `${p}/agents/${slug}` })) },
    {
      text: t.cli,
      items: [
        { text: t.cliInstall, link: `${p}/cli/install` },
        { text: t.cliAgents, link: `${p}/cli/agents` },
        { text: t.cliLocal, link: `${p}/cli/local-models` },
      ],
    },
    {
      text: t.reference,
      items: [
        { text: t.endpoints, link: `${p}/reference/endpoints` },
        { text: t.routing, link: `${p}/reference/routing` },
        { text: t.errors, link: `${p}/reference/errors` },
      ],
    },
  ]
}

function nav(locale: "en" | "zh-TW"): DefaultTheme.NavItem[] {
  const t = LABELS[locale]
  const p = locale === "en" ? "" : "/zh-TW"
  return [
    { text: t.start, link: `${p}/guide/getting-started`, activeMatch: `${p}/guide/` },
    { text: t.app, link: `${p}/app/overview`, activeMatch: `${p}/app/` },
    { text: t.agents, link: `${p}/agents/claude-code`, activeMatch: `${p}/agents/` },
    { text: t.cli, link: `${p}/cli/install`, activeMatch: `${p}/cli/` },
    { text: t.reference, link: `${p}/reference/endpoints`, activeMatch: `${p}/reference/` },
  ]
}

export function defineDocsConfig(edition: DocsEditionOptions = {}): UserConfig<DefaultTheme.Config> {
  return defineConfig({
    base: "/docs/",
    title: SITE_NAME,
    cleanUrls: true,
    lastUpdated: edition.lastUpdated ?? true,
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
          nav: [...nav("en"), ...(edition.nav?.en ?? [])],
          sidebar: [...sidebar("en", edition.appPages?.en ?? []), ...(edition.sidebar?.en ?? [])],
          outline: { label: "On this page" },
        },
      },
      "zh-TW": {
        label: "繁體中文",
        lang: "zh-TW",
        link: "/zh-TW/",
        description: DESCRIPTION_ZH,
        themeConfig: {
          nav: [...nav("zh-TW"), ...(edition.nav?.["zh-TW"] ?? [])],
          sidebar: [
            ...sidebar("zh-TW", edition.appPages?.["zh-TW"] ?? []),
            ...(edition.sidebar?.["zh-TW"] ?? []),
          ],
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
}
