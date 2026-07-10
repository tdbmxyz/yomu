# Script-embedded page lists — design

## Problem

Some sites' chapter pages don't render the page images into the DOM at
all: the reader script is handed an inline JSON payload (e.g.
`run({... "images": ["…jpg", …]})`) and injects the `<img>` tags at
runtime. The selector engine's `pages.image` CSS rule finds nothing
there.

## Change

`[pages]` in a source definition gains an optional `images_json`: a
regex, applied to the raw chapter-page HTML, whose first capture group
must be a JSON array of image-URL strings. Example:

```toml
[pages]
images_json = '"images"\s*:\s*(\[[^\]]*\])'
```

- `image` becomes optional. A definition must set `image` or
  `images_json` (Definition error otherwise); when both are set,
  `images_json` is tried first and `image` is the fallback for pages
  where the regex finds nothing.
- The captured text is parsed with serde_json (so `\/` escapes are
  handled for free); each string joins against the page URL like
  selector-extracted values. No matches / an empty array falls through
  to the `image` selector when present, else a Parse error.
- `pages.url` composes unchanged (the regex just runs on the fragment
  the engine fetched).

## Testing

Unit tests beside the existing selector tests: definition with only
`images_json` parses pages out of a `run({...})` script snippet
(escaped slashes included); neither field set is a Definition error;
both set prefers the JSON and falls back to the selector when the
script is absent.

## Rollout

Engine (yomu-server binary) only — definitions live outside the repo.
No config, no client work.
