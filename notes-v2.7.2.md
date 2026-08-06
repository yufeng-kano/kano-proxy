Fixes Claude Code sessions failing against `codex/*` models with `400 Invalid 'prompt_cache_key': string too long`.

Claude Code identifies each session with an id around 150 characters long. The proxy passed that id straight through as the upstream prompt cache key, which accepts at most 64 characters, so every turn of an affected session was rejected before it reached the model. The id is now shortened to a stable, session-specific value before it goes upstream — usually the session's own UUID — so prompt caching keeps working and separate sessions stay on separate cache entries.
