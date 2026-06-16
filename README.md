# Snatch

Clipboard-driven crawler. Copy a URL → it's matched against your sources and
auto-extracted (images, text, or structured data → CSV).

## Usage

1. Run `snatch` (or `cargo run`) — a terminal dashboard opens
2. Add a source: drop a TOML file in `sources/` (one site per file) — see [docs/source-format.md](docs/source-format.md)
3. Copy any matching URL to the clipboard — it's detected automatically
4. Watch tasks in the dashboard; content lands in `~/Desktop/Snatch/`

Keys: `q` quit · `↑↓` select task · `Enter` open its folder · `c` clear finished

## Source types

One TOML file per site under `sources/`, with `type` =

- **data** — rows + columns → CSV (listings, tables, link dumps)
- **text** — novels/news/forums; `single` / `sections` / `chapters` strategies → txt
- **image** — galleries/manga; CSS selectors, container scoping, detail-page following → download

Shared: pagination (query / path / next-page), per-field `get = "text|html|@href"` +
regex purify, trad→simp (zhconv), SQLite dedup. Dashboard is a ratatui TUI with a live
task queue, animated progress bars, and a log pane; clipboard watching is event-driven
(no busy polling on Windows/X11). Cross-platform: Windows · macOS · Linux.

## Example source

```toml
# sources/example.toml — export a link listing to CSV
name = "Example List"
type = "data"
domains = ["example.com"]
match = "/list/"

[data]
row = "li.item"
[[data.columns]]
name = "title"
selector = "a"
get = "text"
[[data.columns]]
name = "url"
selector = "a"
get = "@href"
```

See [docs/source-format.md](docs/source-format.md) for the full format and
[docs/rule-engine-plan.md](docs/rule-engine-plan.md) for the roadmap (XPath / JSONPath / JS).

## Build

```bash
cargo build --release
```

Requires: cmake, perl, clang (for BoringSSL / wreq TLS).

## License

MIT
