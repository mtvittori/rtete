# Files

- [Desktop GUI (iced)](gui.md) - Architecture of the rondine-gui binary — the dedicated tokio runtime bridging backends to iced, the Telegram/WhatsApp environments with parked state, unified feed and solo instances, single-instance hand-off, in-window login and pairing, settings, themes (including Omarchy) and desktop notifications.
- [Terminal graphics (kitty and sixel)](terminal-graphics.md) - How the Rondine TUI shows stickers and photos inside the terminal — environment-based protocol detection, cell-size measurement, the kitty transmit-once/place-per-frame model and the sixel damage-tracking renderer with its encoding cache.
- [Terminal UI (ratatui)](tui.md) - How the rondine TUI binary starts, runs its single select loop over terminal events and backend responses, renders with ratatui plus image placements, handles keyboard, mouse and paste input, and always restores the terminal on exit or panic.
