# Roadmap

What is done, what is not, and which formats are worth adding next.

## Done

| Phase | |
|---|---|
| 1–4 | Skeleton, common data model, handler architecture, content-first detection |
| 5–11 | SL1: parse, validate, metadata, layers, preview, write, round trip |
| 12–13 | GOO: parse and write, SL1 ↔ GOO both directions |
| 14–17 | Interface, layer preview, batch conversion, history and settings |
| 18 | Command line, sharing the engine |
| 19–20 | CTB and PHZ: read and written, verified against real UVtools files |
| — | UVJ: read and written, verified lossless against UVtools both ways |
| — | WatchDog: watch a folder, convert what lands in it, deliver where told |

## Not done

Listed plainly rather than buried, because a roadmap that only lists wins is
not a roadmap.

| Phase | | Notes |
|---|---|---|
| 21 | Packaging | deb done, with dependencies read from the binary. Flatpak, AppImage, rpm, AUR not started. |
| 22 | Final polish | Ongoing. See "Known and not yet fixed" below. |

### Known and not yet fixed

Small, real, and written down so they are not carried around in somebody's
head.

- ~~**The guide is a blank page.**~~ Written. Five walkthroughs in a panel
  on the right, opened from the rail. The blank section in About goes when
  the two About surfaces below are settled.
- **Two About surfaces.** There is an About dialog on the rail and an About
  section in Settings, and the Settings one is the better of the two - it
  lists the formats and where the settings file lives, which the dialog does
  not. Worth deciding which is the real one rather than maintaining both.
- **The page runs off the right edge below about 480px.** The chain copes;
  the rows and the Clear button on the WatchDog page do not. A real window
  manager will not let a window go below its minimum, so this shows up when
  tiling into a narrow column.

Built but never run against the hardware, which is a different kind of not
done:

- **The eject button.** It only appears for a removable drive that is present,
  so it has never been exercised.
- **The 0.03 mm exposure profile** (2.1s) is unvalidated, and the bus tilt fix
  has not been printed.

Smaller pieces of the specification still outstanding:

- **3D layer view.** Not in the specification, but asked for. A stacked view of
  subsampled layers, since a full voxel model of a real print is billions of
  voxels.
- **UVtools refuses two synthetic resolutions**, 112x56 and 128x64 at four
  layers, whoever wrote the file — catibo's included. No printer has a panel
  that size. Recorded rather than chased.

Done since this list was last written, and recorded here because a roadmap
that quietly deletes its own entries is not much of a record:

- **Architecture documentation** (§45). `docs/ARCHITECTURE.md`.
- **Open the converted file** (§27). The toast opens the file, falling back to
  its folder when nothing on the system claims the format.
- **File associations** (§33). MIME definitions are installed and
  `xdg-mime query filetype` now identifies all three formats.
- **GOO previews.** Read as well as written, so a conversion keeps its picture.
- **Manual input format override** (§21). The Input control on the Convert
  page, and `--input-format` on the command line.

## Which formats next

There are roughly thirty mSLA formats in circulation. They are not equally
worth having.

### Tier 1 — the biggest gap

**CTB** (`ctb`, `cbddlp`, `photon`, `gktwo.ctb`)

Chitubox is the most widely used resin slicer, and CTB is what it writes.
Between them the Chitubox family covers Elegoo's earlier Mars and Saturn
printers, most of Anycubic's range, Phrozen, and a good deal else. If only one
more format is ever added, this is the one.

Five revisions exist and the layout differs between them. An encrypted variant
also exists for some printers; that is a separate, murkier problem and is worth
treating as out of scope rather than half-supported.

**PHZ**

Chitubox's older output. Structurally close to CTB, so it is cheap once CTB is
understood, and it keeps older machines working.

### Tier 2 — cheap and disproportionately useful

**UVJ** — done. A ZIP of PNG layers and a JSON manifest, verified lossless
against UVtools in both directions.

**PNG image stack** (`png`, and the other image types) — **not worth doing,
and here is why, so it does not get picked up again.**

The argument for it was that exporting layers as numbered images is nearly
free, and that it turns CheapAzSLA into something you can point at a file to
find out what is actually in it — the most useful thing possible when a print
fails and nobody can say why. That argument was written before UVJ landed, and
UVJ is exactly that: one 8-bit greyscale PNG per layer, plus a manifest, read
and written. SL1 is the same thing again, since a PrusaSlicer file is already a
ZIP of numbered greyscale PNGs.

So the job is done twice over. Convert to UVJ, unzip it, and page through
`slice/00000000.png` onwards in any image viewer. A folder of loose PNGs would
save the unzip step and nothing else, which is not a format's worth of work.

**OSLA**

An open mSLA format with a published specification. Same argument as UVJ.

### Tier 3 — large installed bases

**Anycubic** (`pws`, `pwmx`, `pm3`, `pm5`, `pwma` and around twenty more)

Anycubic's Photon line is one of the largest installed bases in resin
printing, and one handler covers all twenty-two extensions because they share a
container with a version field. High value per unit of work — but more work
than it looks, and here is what a first look established, so the next attempt
starts further along.

The container is sections, not a fixed header:

```
0x00  char[12]  "ANYCUBIC" and padding
0x0C  u32       version: 1, or 515 to 518 for the newer machines
0x10  u32       how many sections follow
0x14  u32[]     one offset per section, at a stride of eight bytes
                (four bytes used, four skipped, the last one not padded)

each section:
      char[12]  name: HEADER, PREVIEW, LAYERDEF, and the layer data
      u32       payload length
      payload
```

The version 1 HEADER is 80 bytes and legible: pixel size in micrometres,
layer height, exposure, bottom exposure, bottom layer count as a float, lift
height, then speeds **in millimetres per second** rather than per minute like
every other format here, resolution, and resin mass.

Two things make this more than an afternoon. The five versions differ
structurally, and version 1 is the old Photon while 515 to 518 are what
current machines read — supporting only version 1 would cover the extensions
and none of the printers. And the newer variants identify the machine from its
resolution and refuse a file whose panel size matches nothing they know, so
they cannot be tested against synthetic sizes the way every other format here
was; they need a real panel size for a real Anycubic printer.

Worth doing, worth doing properly, and not worth doing halfway.

**Creality CXDLP** (`cxdlp`, `cxdlpv4`)

Creality's Halot range. Worth noting that this format identifies the printer
model from the resolution, so a file whose resolution matches no known machine
cannot be written at all. That is a real constraint, not a bug to fix.

### Tier 4 — support when asked for

Longer LGS, Anet N4 and N7, Voxelab FDG, UnizMaker ZCode, Uniformation JXS,
Zortrax ZCodex, Makerbase MDLP and GR1, FlashForge SVGX, Emake3D QDT, NanoDLP,
NovaMaker CWS, Klipper ZIP, Voxeldance VDT.

Each is one handler and a set of tests. None is hard once the architecture is
in place, which is rather the point of the architecture. They are simply not
worth doing before someone has a printer that needs one.

### Deliberately not planned

- **Encrypted CTB.** Reading it turned out to be in scope after all: the
  obfuscation is a published XOR stream, versions 4 and 5 use it as a matter of
  course, and refusing them would have meant refusing most real files. Writing
  deliberately does not use it, since the slicer's own opt-out is to set the
  key to zero.
- **Anything that is not resin.** No FDM, no G-code. See the scope section of
  the README.

## Order

CTB, PHZ and UVJ are done, and the PNG stack turned out to be covered by UVJ.
What is left, in order:

1. Packaging, because software nobody can install is not finished
2. Final polish — the list above, and the guide that is still a blank page
3. The 3D layer view
4. Anycubic, when there is a real Anycubic panel size to test against
5. Everything else, on request

Adding a format is one handler, one line in the registry, and its tests. See
[ADDING_A_FORMAT.md](ADDING_A_FORMAT.md).
