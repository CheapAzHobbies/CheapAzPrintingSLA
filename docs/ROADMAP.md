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

## Not done

Listed plainly rather than buried, because a roadmap that only lists wins is
not a roadmap.

| Phase | | Notes |
|---|---|---|
| 19 | CTB | Done. Reading verified against real UVtools files v3-v5; writing verified by UVtools reading it back at every real printer resolution. |
| 20 | PHZ | Legacy Chitubox, closely related to CTB. |
| 21 | Packaging | deb done, with dependencies read from the binary. Flatpak, AppImage, rpm, AUR not started. |
| 22 | Final polish | Ongoing. |

Smaller pieces of the specification still outstanding:

- **Architecture documentation** (§45). `ADDING_A_FORMAT.md` and
  `TEST_PLAN.md` exist; a document describing the engine as a whole does not.
- **3D layer view.** Not in the specification, but asked for. A stacked view of
  subsampled layers, since a full voxel model of a real print is billions of
  voxels.
- **UVtools refuses two synthetic resolutions**, 112x56 and 128x64 at four
  layers, whoever wrote the file — catibo's included. No printer has a panel
  that size. Recorded rather than chased.

Done since this list was last written, and recorded here because a roadmap
that quietly deletes its own entries is not much of a record:

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

**PNG image stack** (`png`, and the other image types)

Exporting layers as numbered images is nearly free given the engine already
decodes every layer to greyscale, and it turns CheapAzSLA into something you
can point at a file to find out what is actually in it. It is also the most
useful thing possible when a print fails and nobody can say why.

**UVJ**

An open interchange format: a ZIP of PNG layers and a JSON manifest. Simple,
documented, and the natural lossless intermediate. Valuable for testing as
much as for users.

**OSLA**

An open mSLA format with a published specification. Same argument as UVJ.

### Tier 3 — large installed bases

**Anycubic** (`pws`, `pwmx`, `pm3`, `pm5`, `pwma` and around twenty more)

Anycubic's Photon line is one of the largest installed bases in resin
printing, and one handler covers all of those extensions because they share a
structure with a version field. High value per unit of work, though the
version differences need care.

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

- **Encrypted CTB.** Working around a format's encryption to write files for
  it is a different kind of project with a different set of questions attached.
- **Anything that is not resin.** No FDM, no G-code. See the scope section of
  the README.

## Order

1. CTB, because it unblocks the most printers
2. PNG image stack, because it is nearly free and immediately useful
3. Packaging, because software nobody can install is not finished
4. PHZ, riding on CTB
5. The 3D layer view
6. Anycubic
7. Everything else, on request

Adding a format is one handler, one line in the registry, and its tests. See
[ADDING_A_FORMAT.md](ADDING_A_FORMAT.md).
