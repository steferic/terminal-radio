# terminal-radio

<p align="center">
  <img src="assets/terminal-radio-demo.gif" alt="terminal-radio — a braille globe of internet radio in your terminal" width="600">
</p>

A braille-rendered orthographic globe of internet radio stations you spin between,
right in your terminal — pick a spot on the planet and listen. It pulls verified,
geolocated stations from radio-browser.info and renders the globe with Unicode
braille.

> Sibling project: the original browser/e-ink globe prototype this grew out of lives
> in a separate repo (`world-radio`).

## Install

The installed command is **`radio`**. For audio you'll also want `mpv` or `ffmpeg`
on your PATH.

**Prebuilt binary — no Rust needed** (macOS / Linux):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/steferic/terminal-radio/releases/latest/download/terminal-radio-installer.sh | sh
radio
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/steferic/terminal-radio/releases/latest/download/terminal-radio-installer.ps1 | iex"
```

**With Rust** (cargo):

```sh
cargo install terminal-radio    # from crates.io → installs the `radio` command
# or straight from source:
cargo install --git https://github.com/steferic/terminal-radio
```

(The prebuilt installers/URLs become live once the first version tag is released — see
[Releasing](#releasing).)

## Run from a clone

```sh
cargo run --release         # spin the globe
cargo run -- --snapshot     # render one frame as text (no TTY needed) — used for tests
```

![one frame](snapshot is printed by `--snapshot`)

## Controls

| Action | Keys |
|---|---|
| Next / previous station | `→`/`←`, `l`/`h`, or `j`/`k` |
| **Random station** | `x` |
| **Filter** by continent / country | `f` |
| **Globe size** (zoom the earth in / out) | `]` / `[` (also `+` / `-`) |
| Play / stop stream | `p` or `Space` (best-effort; needs `mpv` or `ffmpeg` installed) |
| **Radio filter** (lo-fi AM-radio sound) | `r` |
| **Maximize globe** (full screen → more braille detail) | `z` |
| Toggle country borders | `b` (default on) |
| Toggle color | `c` (default on) |
| Toggle graticule | `g` (default off) |
| Reload / re-verify stations | `L` |
| Quit | `q` or `Esc` |

These are always visible in-app too: a **Controls panel** sits under the station list
and doubles as a live status readout (it shows the current `radio fx` / `borders` /
`color` / `grid` states and updates as you toggle them).

## Filtering (`f`)

`f` opens a popup to narrow the list by **continent** or **country**. Each section
lists the values actually present (with station counts); pick one to filter, or
"All …" / "Clear all filters" to reset. The two **stack** (e.g. *Europe* + *France*),
and the globe markers + list + random all narrow to the matches. Continent is derived
from the station's country code; country from radio-browser's metadata. ↑/↓ to move,
Enter to apply, Esc to close.

## Radio filter (`r`)

`r` toggles a lo-fi **AM/shortwave radio** sound on the audio: a narrow band-pass
(≈300–3400 Hz, so no real bass or treble), broadcast-style compression, a touch of
bit-crush grit, and a little makeup gain. It's applied via the spawned player's
audio-filter option (mpv `--af` / ffplay `-af`, an ffmpeg/libavfilter chain — see
`RADIO_AF` in `main.rs`), so it needs **mpv** or **ffmpeg** installed. Toggling it while
a stream is playing re-spawns the stream with/without the filter. (cvlc plays clean — no
simple CLI filter path.)

The globe renders purely in **braille** — fast text cells. There's no image/graphics-
protocol mode; for finer detail see below.

## Why braille + 50m data (precision)

A terminal isn't limited to 1-bit pixels. Unicode braille (`⠿`) packs a **2×4 grid
of dots per character cell** — finer than half-blocks (1×2) or sextants (2×3) — so the
globe is drawn at the maximum spatial resolution a terminal glyph allows. ratatui's
`Canvas` widget rasterizes lines/points to braille for us; the coastline polylines and
station markers project into canvas space via a small orthographic-projection core.

The other half of precision is the **source data**: this uses Natural Earth **50m**
coastlines and country borders (≈60k + 20k points) — roughly 4× the detail of the
110m set, which is about the resolution a braille globe can actually resolve (10m would
be invisible at terminal size and bloat the binary).

### Making braille finer
A braille dot is exactly 1/8th of a character cell, so **dot size = your terminal font
size**, and the globe's resolution = (cells across) × (2 or 4 dots). Two ways to get
2–4× more detail while keeping the fast text rendering:
- **Shrink the terminal font** (Ghostty: `⌘ −`) — smaller dots *and* more cells.
- **Maximize the globe** with `z` — hands the whole terminal to the globe so braille
  gets the most cells. Combine both for a very crisp globe.

**Country borders** (Natural Earth 50m admin-0 land boundaries) draw as a `b`-toggleable
layer. In mono they use a brightness hierarchy — coastlines bright white, borders dim
gray, graticule dimmest; in color, coast cyan / borders tan / land green.

**Dirty-flag redraw:** the screen only repaints when something changes, so the 50m
detail costs ~zero CPU at idle. `c` toggles color (on by default); `g` toggles the
lat/long graticule (off by default).

## Only-working stations (verified)

**Every station shown is verified playable** — the list never contains dead streams or
placeholder entries — and it loads **hundreds** of them worldwide. Verification runs
**automatically on startup** (and again on `L`):

1. Pull ~2,000 geolocated stations from [radio-browser.info](https://www.radio-browser.info)
   (popularity-ordered, resolved direct URLs). It has ~12k geolocated stations; we dedupe
   by URL only — *not* by location — so many stations per city survive, the way Radio
   Garden stacks dozens on one place. The fetch tries multiple mirrors (`all.api`, `de1`)
   and **retries**, since the service rate-limits / 503s under load (that's what used to
   leave you with only the curated few).
2. **Verify each with `ffprobe`** — it actually opens the stream and confirms a decodable
   audio track, using the *same* libav engine as the player, so a pass means it really
   plays (far stricter than an HTTP status/content-type check). 24 concurrent workers,
   probing the ~800 most popular.
3. Stations **stream into the list the moment they pass** (up to 300), so the globe fills
   up live — early-stops once the target is hit.

The curated few (all known-good) show instantly so the app is usable while the full set
loads in the background — watch the `Verifying… N/M checked, K working` counter. If
radio-browser is fully unreachable, it keeps the curated few and says so.

> **Caveat:** this verifies a stream *plays at load time*. A station can still drop
> mid-listen, or loop a recorded "we're offline" message (valid audio ffprobe can't
> distinguish). Re-run with `L` to re-verify. Tuning knobs: `VERIFY_TARGET`,
> `PROBE_CAP`, `PROBE_WORKERS` in `main.rs`. Needs `ffmpeg` installed (falls back to an
> HTTP check otherwise).

## Architecture

```
terminal-radio/
├── Cargo.toml
└── src/
    ├── globe.rs       # projection core — no ratatui/IO deps (pure math)
    ├── coastline.rs   # baked Natural Earth 50m coastlines (generated data)
    ├── borders.rs     # baked Natural Earth 50m country borders (generated data)
    ├── stations.rs    # curated fallback station table
    └── main.rs        # ratatui app: layout, braille Canvas, input, audio, live verify
```

`globe.rs` is deliberately dependency-free (just projection math); everything
ratatui/terminal-specific stays in `main.rs`. The `coastline.rs` / `borders.rs` data
modules are pre-generated and committed, so the crate builds with no external assets.

## Regenerating the geo data

`coastline.rs` and `borders.rs` are baked from [Natural Earth](https://www.naturalearthdata.com/)
50m vector data (`ne_50m_coastline` and `ne_50m_admin_0_boundary_lines_land`, as GeoJSON).
They rarely need regenerating; if you do, fetch the GeoJSON and emit each line as a
`&[(f32, f32)]` of `(lon, lat)` points into the corresponding `const` array.

## Releasing

Prebuilt binaries + installers are produced by [dist](https://opensource.axo.dev/cargo-dist/)
(config in `dist-workspace.toml`, CI in `.github/workflows/release.yml`). To cut a release,
tag a version and push the tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions then cross-compiles `radio` for macOS (arm64/x64), Linux (arm64/x64), and
Windows (x64), and publishes a GitHub Release with the `.tar.xz`/`.zip` artifacts,
checksums, and the `curl … | sh` / PowerShell installers referenced above.

## Roadmap

- [x] Braille globe + snap-to-station navigation + station list
- [x] Mono + color render styles
- [x] Live stations from radio-browser.info, with concurrent stream verification
- [x] Maximize toggle + radio (lo-fi AM) audio filter
- [ ] Shaded style; more palettes
- [ ] Robust audio (rodio/symphonia instead of spawning mpv)
- [ ] Search / filter, favorites
