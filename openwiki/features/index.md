# Files

- [Interface languages (i18n)](i18n.md) - How Rondine localises its UI with Italian source strings as keys, the tr()/trf! helpers, the two-line locales/<lang>.txt format embedded at compile time, language detection, and the test that enforces complete translations.
- [Downloads, playback, clipboard and links](media-actions-and-clipboard.md) - Backend-neutral user actions in Rondine — saving media to Downloads, audio/video playback through an external player, clipboard copy and paste of text, images and files, Telegram link recognition and resolution, and locally stored starred messages.
- [Message translation](translation.md) - How Rondine translates chat messages with DeepL or an OpenAI-compatible service (Groq, OpenRouter, Ollama) — configuration file, request flow through the worker, manual toggle versus per-chat automatic translation, error handling and the settings test phrase.
