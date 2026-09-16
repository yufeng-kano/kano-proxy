import { defineDocsConfig } from "./site"

/** The standalone site: the core's pages and nothing else (docs/docs-site.md § Editions). */
export default {
  ...defineDocsConfig(),
  // Content lives under pages/ so the app root holds only the app's own files.
  srcDir: "pages",
}
