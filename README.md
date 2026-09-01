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

### Conversion runs on every core it can use

Decoding a source layer and re-encoding it for the destination is the whole
cost of a conversion, and each layer is independent of every other. The file
is not: records have to reach it in order. So the expensive half runs on a
pool and the results are handed back in index order to a single writer, which
keeps the append-only, atomic-rename property the conversion depends on.

How many layers are worked on at once is decided from the machine, not fixed:
core count, capped by how much memory a layer costs and by what is actually
free right now. A 11520x5120 panel is 59MB per layer, and thirty-two of those
in flight is 1.9GB on a machine that may not have it. Workers are also held to
a short lookahead, so a fast decoder cannot queue an entire print into memory
while the writer catches up.

A 438 layer, 11520x5120 print converts from SL1 to GOO in 8.3 seconds on a
32-core machine, against 48.1 seconds for the same work done one layer at a
time. The output is byte-identical apart from the timestamp in the header.

## Current state

| Area | State |
|---|---|
| Common print model | done, tested |
| Safety limits for untrusted input | done, tested |
| Lazy layer provider and cache | done, tested |
| Ordered parallel conversion pipeline | done, tested |
| Format handler interface and registry | done |
| Format detection by content | done, tested |
| SL1 reading | done, tested against real slicer output |
| SL1 writing | not started |
| GOO reading and writing | done, verified against a real Elegoo file |
| CTB reading and writing | done, verified against real files both ways |
| PHZ | not started |
| Desktop interface | done |
| Command line | done |
| Packaging | not started |

Anything marked not started is absent, not stubbed. There are no buttons that
do nothing.

CTB is the line above worth expanding on, because it was verified in both
directions and one of those found something.

**Reading.** UVtools, the reference implementation for these formats,
converted a real 438 layer print at 11520x5120 to CTB at versions 3, 4 and 5,
and this reader gives back pixel data identical to the source for every layer
of each — the same lit-pixel counts, the same checksums. Versions 4 and 5
obfuscate their layer data, so the cipher is verified against real files too.
Two files written by [catibo](https://github.com/cbiffle/catibo) are committed
under `crates/core/tests/data` and read in the test suite down to individual
pixels.

**Writing.** The same print written back out as CTB, read by UVtools and
converted to SL1 again, comes back with every lit pixel in the same place as
the original. Files written here are read by UVtools at every resolution a
real printer has: 1440x2560, 2560x1620, 3840x2400, 4098x2560, 5760x3600 and
11520x5120.

That took working out, and the answer was not where it looked. Chitubox writes
an 84 byte record in front of every layer's data, repeating that layer's table
entry and its motion. UVtools reads that record whether it is there or not, so
a file without it is read as though whatever bytes happen to precede the
payload were that record — which produces impossible layer heights and a
refused file, for some resolutions and not others depending on what those
bytes happen to be. The tell was that UVtools refuses catibo's files in exactly
the same cases, and catibo is an implementation its author verified by printing
from it. A writer whose output is rejected by the same tool that rejects a
known-good implementation's output is not obviously the one at fault.

Two synthetic sizes are still refused by UVtools — 112x56 and 128x64, four
layers — and refused identically whoever wrote them. No printer has a panel
that size and no slicer produces one, so it is recorded here rather than
worked around.

A file from Chitubox itself would still be worth having, and the test for one
is written and skips until it exists:

    CHEAPAZSLA_REAL_CTB=/path/to/from-chitubox.ctb cargo test -p cheapazsla-core

What is coming and in what order is in [docs/ROADMAP.md](docs/ROADMAP.md),
including which of the thirty-odd resin formats are worth adding and which
are not.

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
git clone https://github.com/CheapAzHobbies/CheapAzPrintingSLA.git
cd CheapAzPrintingSLA
cargo build --release
cargo test
```

The application is `CheapAzSLA`; the repository is `CheapAzPrintingSLA`, which
keeps it beside the other CheapAzPrinting work.

Tests that need a real slicer file are skipped unless you point at one:

```bash
CHEAPAZSLA_REAL_SL1=/path/to/a/real.sl1 cargo test
```

A converter that only reads files it wrote itself proves very little, so the
suite is built to run against genuine slicer output.

## Installing

On Debian, Ubuntu, Mint and anything else that takes `.deb`:

```bash
tools/make-deb.sh
sudo apt install ./target/cheapazsla_0.1.0_amd64.deb
```

The package declares what it needs rather than bundling it. GTK4 and
libadwaita are in every distribution new enough to have them, and a bundled
copy of a toolkit is a second one to keep patched. Its dependency list is not
written by hand either: the script reads the binary's own `DT_NEEDED` entries
and asks the package manager which package owns each, so the list cannot drift
away from what the program actually links against.

It also installs the MIME definitions, so a print file opens in CheapAzSLA
when double-clicked, and refreshes the icon and desktop caches afterwards.

Anywhere else, or to run from a working copy:

```bash
tools/install.sh          # into ~/.local, no root needed
```

Flatpak, AppImage, rpm and an AUR package are not done.

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

- **[catibo](https://github.com/cbiffle/catibo)** by Cliff L. Biffle is a Rust
  implementation of CTB, CBDDLP and PHZ, and its `doc/cbddlp-ctb.adoc` is the
  clearest description of the CTB layout in the open. The reader here was
  written against it and checked field by field against it, including the
  layer cipher, whose constants that document records to the bit. Where its
  prose and its code disagreed on one of those constants, the code was right.

  Two files its encoder produced are committed under `crates/core/tests/data`
  and read by the test suite, which is what lets the CTB tests check this
  reader against an understanding of the format that is not its own. catibo is
  BSD-2-Clause, Copyright (c) 2020 Cliff L. Biffle.

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
