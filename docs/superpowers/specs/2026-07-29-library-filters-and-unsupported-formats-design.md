# Library filters and unsupported local formats

Three changes, driven by a real 25-publication local import: the library
forgets which category you were looking at, the Home "New chapters" shelf
drowns in a bulk import, and folders holding formats the streamer cannot
read either vanish or — worse — import as something broken.

## 1. The library remembers its category tab

The category row ("All | Reading | Paused | Finished") resets to All on
every visit. A reader who lives in Reading re-selects it every time.

The kind switcher already solves exactly this problem: `library_kind()` /
`set_library_kind()` in `offline.rs` persist the selection under
`yomu-library-kind` and fall back when the stored value no longer has
content. The category tab follows that precedent.

- `offline::library_category() -> Option<String>` reads
  `yomu-library-category`; a missing key or the empty string means All.
- `offline::set_library_category(Option<&str>)` writes it, storing the
  empty string for All.
- The library page seeds its category signal from the stored value and
  writes on every tab click.
- If the stored id is not in the server's category list (a category was
  renamed or removed), fall back to All rather than showing an empty
  grid against a tab that does not exist.

Persistence is per device, like the kind. Two devices can sit on
different tabs, which is the point — the phone is for Reading.

## 2. "New chapters" follows the same category

`home.rs` builds its `fresh` shelf from every entry with
`unread_count > 0`, sorted by `latest_unit_at` descending, truncated to
12. A bulk local import lands 25 publications in `reading` with every
unit unread and `latest_unit_at` set to the import instant, so they take
all 12 slots and the shelf stops meaning "new".

The shelf applies the same stored category filter as the library. Pick
Reading and Home shows Reading. This adds no new control: one selection,
made in one place, honoured in both.

Deliberately unchanged:

- **"Continue reading" is not filtered.** It only lists publications with
  a locator — things actually in progress — and is not subject to the
  same flooding.
- **`update_enabled` is not reused as the filter.** It is the updater's
  "may I poll this source" flag, and every default category that a reader
  would want on Home (`reading`) has it on, so it does not discriminate.

## 3. Unsupported formats are shown, not faked

`discover_entry` in `streamer/files.rs` classifies a top-level directory
by what it holds: unit directories or `.cbz` archives make it a series;
loose images make it a single-unit publication; anything else is skipped
with one log line. Two gaps in that classifier produce entries that look
imported but are not readable.

**A lone `cover.jpg` counts as loose images.** A folder of `.cbr` or
`.epub` volumes plus the cover art that tachidesk left behind has no
units and one image, so it becomes a single-unit publication whose only
chapter is the cover. Observed on Death Note (13 cbr), Ranma ½ (38 cbr),
L'habitant de l'infini (30 cbr) and Les Carnets de l'apothicaire
(13 epub) — each a one-chapter, one-page publication.

**Any subdirectory counts as a unit.** A folder whose subdirectories hold
only `.pdf` files becomes a series with one unit per subdirectory, and
every one of those units 404s on `/pages`. Observed on Dragon Ball
Perfect Edition: 34 chapters, none openable, no cover.

### The classifier

- `cover.*` (any image basename `cover`) no longer contributes to the
  loose-image decision. `cover.jpg` alongside real page images still
  yields a single-unit publication; `cover.jpg` alone does not.
- A subdirectory counts as a unit only if it contains at least one image
  or a `.cbz`. This costs one extra `read_dir` per candidate unit
  directory, paid only by series that have subdirectories at all — the
  common flat `<Series>/<Chapter>.cbz` layout is untouched.

### The flag

Every file the scan passes over because of its extension is counted
against the publication that contains it, whether the publication has
readable units or not. Black Clover — 19 `.cbz` and 9 `.cbr` in one
folder — imports its 19 volumes and reports the other 9, which is the
case that matters most: today those nine volumes go missing in silence.

Two additive fields on `Publication` and `PublicationWire`:

```rust
/// Files skipped by the scan because their format is unreadable.
pub unsupported_count: u32,
/// Sorted, deduplicated extensions of those files, e.g. ["cbr"].
pub unsupported_formats: Vec<String>,
```

Both are `#[serde(default)]` with `skip_serializing_if` on the zero
value, so the 1.x wire is unchanged for 1.x clients: they ignore fields
they do not know, exactly as they already do for `missing_since`.

A directory whose files are *all* unsupported is imported with zero
units and its flag set, rather than skipped. Yu-Gi-Oh (19 pdf) and
Dragon Ball (34 pdf) return to the library as visible, explained
entries. `cover.jpg` still supplies `cover_url` where one exists, so
they stay recognisable on the shelf.

Unchanged: hidden entries (`.name`) and root-level loose files are still
skipped silently. The interrupted-download leftovers tachidesk leaves
behind (`.Foo.cbz.0azQdC`) are hidden files and must stay invisible.

### Storage

Migration `0013` adds to `publications`:

```sql
unsupported_count   INTEGER NOT NULL DEFAULT 0,
unsupported_formats TEXT    NOT NULL DEFAULT ''
```

`unsupported_formats` is stored comma-joined. Existing rows default to
"nothing unsupported", which is correct for every source-origin
publication and gets corrected for local ones on the next scan.

### UI

A badge beside the title, in the same slot and style as the existing
`missing-badge`:

> unsupported: 9 cbr

on the publication page, and a marker on the library grid tile matching
how `class:missing` is applied today. A publication with zero units and
a flag reads as "yomu can see this folder but cannot open its files",
which is the honest statement.

## Testing

The streamer fixture builder in `streamer/mod.rs` already writes `.cbz`
archives and loose files, so these are unit tests, not integration ones:

- cover-only folder plus `.cbr` files → one publication, zero units,
  `unsupported_count == 2`, `formats == ["cbr"]`, `cover_url` set
- subdirectories holding only `.pdf` → zero units, `count == n`,
  `formats == ["pdf"]`
- `.cbz` and `.cbr` side by side → units for the `.cbz` only, the `.cbr`
  counted
- loose images *including* `cover.jpg` → still one unit, and the cover
  still becomes `cover_url`
- mixed unsupported extensions → formats sorted and deduplicated
- hidden partial download → still invisible, not counted

Each of these must go red if its guard is removed — the cover-only test
in particular fails today by producing a one-page publication.

For the UI: `library_category` round-trips through localStorage and
falls back to All for an id absent from the category list; the Home
shelf filter keeps only entries in the selected category and keeps
everything when the selection is All.
