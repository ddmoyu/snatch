# Snatch

Clipboard-driven crawler for images and novels. Copy a URL → auto-download everything matched by CSS selectors.

## Usage

1. Start `snatch.exe` — green "P" icon appears in system tray
2. Edit `scraper.toml` to add your target sites
3. Copy any matching URL to clipboard
4. Content appears in `~/Desktop/Snatch/`

## Features

- **Images**: CSS selectors, container scoping, detail page following
- **Novels**: Text extraction with paragraph preservation
- **Pagination**: query params, URL path, next-page links
- **Trad→Simp**: MediaWiki/OpenCC via zhconv
- **Dedup**: SQLite, avoids re-downloading
- **Filtering**: exclude selectors, watermark stripping
- **9.4MB** binary, cross-platform

## Example rules

```toml
[[rules]]
name = "Example Image Site"
domain = "example.com"
container = ".gallery"
selectors = [
    { expression = "img", attribute = "src" },
    { expression = "img", attribute = "data-src" },
]

[[rules]]
name = "Example Novel Site"
domain = "novels.example.com"
mode = "text"
content_selector = ".entry-content"
convert = "simplify"
strip = ["watermark-text"]

[rules.pagination]
type = "next_link"
next_selector = "a.next"
max_pages = 20
```

## Build

```bash
cargo build --release
```

Requires: cmake, perl, clang (for BoringSSL / wreq TLS).

## License

MIT
