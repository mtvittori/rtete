# Files

- [App state and sync](app-state-and-sync.md) - How the backend-neutral `App` struct holds chats, history rings, previews and pending work, and how `sync.rs` applies live events and worker responses to it identically for the TUI and the GUI.
- [Architecture overview](overview.md) - Crate layout of Rondine (formerly RTete) — a shared library, a ratatui TUI binary and an optional iced GUI binary — and how frontends, state, workers and the Telegram/WhatsApp backends talk through one request channel and one response channel.
- [Request/Response protocol and neutral model](request-response-protocol.md) - The contract between Rondine's UI/state and its messaging backends — the Request, Response and SendJob enums in send.rs, the backend-neutral types in model.rs (ChatId, ChatRef, MediaRef, HistoryRow, Live), id-space rules, and how to extend the protocol or add a backend.
