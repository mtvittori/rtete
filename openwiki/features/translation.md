---
type: feature
title: Message translation
description: How Rondine translates chat messages with DeepL or an OpenAI-compatible service (Groq, OpenRouter, Ollama) — configuration file, request flow through the worker, manual toggle versus per-chat automatic translation, error handling and the settings test phrase.
tags: [translation, deepl, openai, groq, ollama, ureq, auto-translate]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-9fec9d1af408501c9c455161
    resource: repo://src/app.rs
  - id: openwiki-source-5cbf00031a9c6425a5db3350
    resource: repo://src/events.rs
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-5b1fad98f7b3082039d72750
    resource: repo://src/sync.rs
  - id: openwiki-source-88bf525ee1897df0d3f5c2db
    resource: repo://src/translate.rs
  - id: openwiki-source-89402e3f86a3a076c01e7039
    resource: repo://src/worker.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Message translation

Rondine can translate incoming message text with an external service. This is
independent from the UI's own languages (see
[Interface languages](i18n.md)). Message text is sent to the configured service
only when a translation is requested (README "Traduzione dei messaggi").

## Configuration (`translate::Config`)

| Field | `translate.conf` key | Default |
| --- | --- | --- |
| `provider` (`DeepL` / `OpenAi`) | `servizio = deepl\|openai` | DeepL |
| `deepl_key` | `deepl_chiave` | empty |
| `oa_url` | `openai_url` | `https://api.groq.com/openai/v1` |
| `oa_key` | `openai_chiave` | empty |
| `oa_model` | `openai_modello` | `llama-3.3-70b-versatile` |
| `target` (ISO code, `None` = UI language) | `lingua = auto\|<code>` | auto |
| `auto` (chat ids translated automatically) | `auto = id,id,…` | empty |

The file is `<config>/rondine/translate.conf`, saved with `private_file`
(0600) inside a 0700 directory because it contains API keys (see
[On-disk storage and paths](../concepts/storage-and-paths.md)). Both the TUI
and GUI read it with `Config::load()` at startup; only the GUI edits it
(*Settings → Translation*: provider, keys, presets, target language, "Test").
TUI users edit the file by hand.

`PRESETS` offers three OpenAI-compatible endpoints: Groq, OpenRouter and a local
Ollama (`http://localhost:11434/v1`). `TARGETS` lists twelve target languages.

`Config::ready()` is true when DeepL has a key, or the OpenAI provider has a
key or an URL containing `localhost` (Ollama needs no key).

## Providers (`translate::translate`)

All HTTP calls are synchronous `ureq` requests with a 25 s global timeout and
HTTP status codes read manually (`http_status_as_error(false)`), so error
bodies become readable messages.

**DeepL** — `POST /v2/translate` with `DeepL-Auth-Key`. Keys ending in `:fx`
(free plan) use `api-free.deepl.com`, others `api.deepl.com`. Some targets need
variants (`en`→`EN-US`, `pt`→`PT-BR`, `zh`→`ZH-HANS`). 403, 456 (quota) and 429
map to specific messages. The detected source language is returned.

**OpenAI-compatible** — `POST {oa_url}/chat/completions` with an optional
Bearer key, `temperature: 0`, `response_format: json_object`, and a system
prompt asking for `{"source": "<ISO 639-1>", "text": "<translation>"}` while
keeping tone, emojis, links and formatting. `parse_llm` extracts the first
`{…}` block (so fenced JSON works) and falls back to the raw content with an
unknown source if the model ignores the format.

`Translated::same_as(target)` reports that the message was already in the
target language.

## Request flow

```
TUI Ctrl-G / GUI "🌐 Traduci" ─▶ App::toggle_translation(peer, id)
GUI chat info "auto" toggle   ─▶ Config::set_auto + App::auto_translate(peer, 20)
history arrives / new message  ─▶ auto chats queue (peer, id, auto=true)
                                       │  App.tr_queue
render loop: App::flush_translations ──┘
   └─ not ready → clear queue, status "configure translation"
   └─ for each text row not Busy: translations[(peer,id)] = Busy; Request::Translate
backend dispatcher ─▶ worker::translate_job  (Semaphore(3), spawn_blocking)
   ─▶ Response::Translated { peer, id, auto, target, res }
sync::handle_response:
   same language → TrState::Same (status note only if manual)
   ok            → TrState::Shown
   error         → remove state, status error; if auto → disable auto for that chat
```

- Only text rows are translated (`App::row_text`); in the TUI `Ctrl-G` picks
  the selected text message or the last received one, and pressing it again
  hides the translation (`Shown` ↔ `Hidden` without a new request).
- Opening an auto chat or turning auto on queues the last
  `AUTO_TRANSLATE_LAST = 20` incoming text messages not yet translated; new
  incoming messages in auto chats with a loaded history are queued in
  `sync::handle_live`.
- At most three translations run concurrently per backend to stay within
  service rate limits.
- `Request::TranslateTest` translates the fixed phrase
  `"Hello! This is a test message from Rondine 🙂"`; the GUI shows the result
  with the detected source and target codes.

State details are in [App state and sync](../architecture/app-state-and-sync.md).

## Tests

`translate.rs` tests `parse_llm` (fenced JSON, plain text fallback,
case-insensitive `same_as`) and the DeepL target-code mapping.
