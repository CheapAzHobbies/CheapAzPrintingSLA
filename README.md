# CheapAzSLA

**Resin Print File Converter & Inspector**

A Linux desktop utility for resin/SLA print files. Open a file, look through
its layers to check it is what you think it is, and convert it to another
format.

By [CheapAzHobbies](https://github.com/CheapAzHobbies).

> **Status: in development.** The engine reads PrusaSlicer SL1 files and is
> covered by tests against real slicer output. Writing, the other formats, and
> the desktop interface are being built in order. Nothing here claims support
> it does not have — see [Current state](#current-state).

## What it is for

```
open  →  inspect  →  convert
```

Three things, done properly:

- **Open** a sliced resin file from any printer, identified by its contents
  rather than its file name.
- **Inspect** it. Scrub through the layers and confirm the file actually
  contains the print you expect, before committing an eight-hour job to it.
- **Convert** it to another format, keeping every piece of print information
  that both formats can express, and saying plainly what cannot survive.

## Scope

Resin only. CheapAzSLA is for masked stereolithography and DLP printers, the
kind that cure a photopolymer with a screen or a projector.

Layers are greyscale exposure masks, one byte per pixel, because that is what
they physically are. There is no colour anywhere in the engine and there will
not be. Nothing here touches filament printing: no extrusion, no toolpaths, no
temperatures, no multi-material.

If you have a `.gcode` file, this is the wrong tool.

## What it is not

CheapAzSLA is not a slicer, and will not become one. It does no model
placement, no supports, no hollowing, no repair, no resin or printer profiles,
and it does not talk to printers. Lychee and Chitubox already do that work
well. This is the utility you reach for when you have a sliced file and need it
in a different shape.

It is also not tied to a manufacturer. Formats are peers; none is privileged.

## Design

### One model in the middle

Every reader produces the same internal representation, and every writer
consumes it:

```
input file
    ↓
format detection          contents first, extension second
    ↓
format parser
    ↓
common print model        geometry, exposure, lift, layers, metadata
    ↓
validation
    ↓
format writer
    ↓
output file
```

The alternative — a converter per format pair — costs N² implementations and
N² sets of bugs. Hub and spoke costs one handler per format, and every format
added makes every existing format more useful.

Two rules keep the model honest:

- Anything a format might not record is optional. A missing exposure time is
  represented as missing, never as zero, because zero is a legitimate value.
- Units live in the field names. Formats disagree about millimetres versus
  microns and seconds versus milliseconds, and normalising on read is the only
  way to keep that from leaking into conversions.

### The engine is a library

```
crates/core     the whole engine, no GUI dependencies at all
crates/cli      command line, depends on core
crates/gui      desktop application, depends on core
```

The separation is enforced by the compiler rather than by discipline. The
engine has no toolkit in its dependency graph, so a widget cannot leak into a
parser, and the CLI physically cannot drift into having its own conversion
code. Both frontends are thin.

### Adding a format touches one place

Implement the handler trait, register it, write tests. It then appears in the
interface and on the command line automatically, because both read the list
from the registry rather than keeping their own.

```
implement FormatHandler  →  register  →  add tests  →  it appears everywhere
```

No interface work is required to add a format.

### Input files are untrusted

A print file is data from somewhere else, and it may be malformed by accident
or on purpose. Every parser routes size and offset decisions through a limits
module with checked arithmetic, so a crafted header cannot make CheapAzSLA
allocate unbounded memory or read past the end of its buffer. Malformed input
produces an error a person can act on, never a crash.

This is a large part of why the engine is written in Rust. Binary format
parsers are exactly where bounds and overflow bugs live, and a language that
checks both is worth more here than familiarity.

### Layers are loaded lazily

A print can hold thousands of layers at several megapixels each. Readers hand
back a provider that decodes a layer only when it is asked for, wrapped in a
small most-recently-used cache so scrubbing the preview stays responsive
without holding the whole stack in memory.

## Current state

| Area | State |
|---|---|
| Common print model | done, tested |
| Safety limits for untrusted input | done, tested |
| Lazy layer provider and cache | done, tested |
| Format handler interface and registry | done |
| Format detection by content | done, tested |
| SL1 reading | done, tested against real slicer output |
| SL1 writing | not started |
| GOO, CTB, PHZ | not started |
| Desktop interface | not started |
| Command line | not started |
| Packaging | not started |

Anything marked not started is absent, not stubbed. There are no buttons that
do nothing.

## Building

Needs a Rust toolchain and GTK4 development headers.

```bash
# Debian, Ubuntu, Mint, Pop!_OS, Zorin
sudo apt install libgtk-4-dev libadwaita-1-dev

# Fedora
sudo dnf install gtk4-devel libadwaita-devel

# Arch, Manjaro, EndeavourOS
sudo pacman -S gtk4 libadwaita

# openSUSE
sudo zypper install gtk4-devel libadwaita-devel
```

Then:

```bash
cargo build --release
cargo test
```

Tests that need a real slicer file are skipped unless you point at one:

```bash
CHEAPAZSLA_REAL_SL1=/path/to/a/real.sl1 cargo test
```

A converter that only reads files it wrote itself proves very little, so the
suite is built to run against genuine slicer output.

## Credits

CheapAzSLA would not be possible without work other people did first and
published openly. The format handlers here are written from format
documentation rather than copied from anyone's source, but the documentation
is the hard part and it was not mine to make.

### File formats

- **[Elegoo](https://www.elegoo.com)** publish the specification for the
  `.goo` format used by the Saturn and Mars printers. An open, documented
  format from a printer manufacturer is rarer than it should be, and it is the
  reason `.goo` support here can be correct rather than guessed at.
- **[connorslade/goo](https://github.com/connorslade/goo)** and the
  [mslicer](https://connorslade.com/projects/mslicer) project publish an ImHex
  pattern and notes covering the parts of the `.goo` layout the official
  document leaves implicit, including that the file is big-endian throughout
  and how the layer checksum is formed. Their notes saved a great deal of
  guesswork.
- **[PrusaSlicer](https://github.com/prusa3d/PrusaSlicer)** by Prusa Research
  defines the SL1 format, which is a plain ZIP of PNGs and settings and is by
  some margin the most inspectable resin format.

### Reference implementations

- **[UVtools](https://github.com/sn4k3/UVtools)** by Tiago Conceição is the
  reference implementation for a great many mSLA formats and the tool most of
  us have relied on for years. It was used here to cross-check that files
  CheapAzSLA reads are understood the same way a known-good implementation
  understands them. If you need a format CheapAzSLA does not support yet,
  UVtools very likely already has it.

### Assets

`assets/penguin_saving.png` is the save indicator, carried over from
[lens](https://github.com/CheapAzHobbies/lens). It is the Club Penguin dance,
141 frames, recoloured to a dark silhouette by that project's
`tools/make_penguin_sheet.py`. The dance itself is Disney's; this is a
recoloured silhouette used as a progress indicator, not shipped as artwork.
Replacing it is a matter of dropping in a different sprite sheet with the same
grid.

### Built with

[Rust](https://www.rust-lang.org), [GTK4](https://www.gtk.org) and
[libadwaita](https://gitlab.gnome.org/GNOME/libadwaita), plus the
[gtk4-rs](https://github.com/gtk-rs/gtk4-rs) bindings, and the `zip`, `png`
and `thiserror` crates.

### On correctness

Format support here is written from specifications and verified against real
files produced by other software. Where CheapAzSLA and an established tool
disagree about a file, assume the established tool is right until proven
otherwise, and please open an issue.

## Licence

GPL-3.0-or-later.

The formats themselves are not owned by this project, and nothing here
restricts anyone else from implementing them.
