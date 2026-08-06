OpenRouter model pricing now follows the provider's live catalog, so models such as GLM 5.2 are estimated instead of appearing unpriced when LiteLLM has no matching entry.

OpenRouter-specific rates are kept separate from LiteLLM lookalikes, conditional and incomplete catalog prices remain unknown rather than being guessed, and cached input-write usage is included in estimates.