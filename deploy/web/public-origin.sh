#!/bin/sh
# Runs from nginx's /docker-entrypoint.d/ before nginx starts. The docs are built
# with a placeholder origin in every absolute URL (canonical, hreflang, og:url,
# sitemap.xml, llms.txt) because no hostname lives in tracked files or images;
# this writes the real one in (docs/docs-site.md § The real hostname).
set -eu
root=/usr/share/nginx/html
placeholder=https://origin.invalid

case "${DOMAIN:-}" in
  "")
    # No origin to name: root-relative URLs still work on the page itself, and a
    # robots.txt Sitemap line must be absolute, so it goes.
    origin=""
    sed -i "\#${placeholder}#d" "$root/robots.txt"
    ;;
  *[!A-Za-z0-9.:-]*)
    echo "public-origin: DOMAIN is not a hostname, leaving the placeholder" >&2
    exit 0
    ;;
  *) origin="https://${DOMAIN}" ;;
esac

grep -rlF "$placeholder" "$root" | while read -r file; do
  sed -i "s#${placeholder}#${origin}#g" "$file"
done
