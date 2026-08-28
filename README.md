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

## Licence

GPL-3.0-or-later.
