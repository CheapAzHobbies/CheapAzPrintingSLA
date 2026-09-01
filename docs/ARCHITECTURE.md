# Architecture

How CheapAzSLA is put together and why, for anyone changing it — including me
in six months. [ADDING_A_FORMAT.md](ADDING_A_FORMAT.md) covers the one job
that has its own recipe; this covers everything else.

## Two crates, and the line between them

```
crates/core   the engine: formats, conversion, settings, history.  No GTK.
crates/cli    a thin command line over the engine.
crates/gui    the desktop application.  All the GTK lives here.
```

The engine knows nothing about the interface. That is checked rather than
trusted: a CI job fails if a GUI toolkit ever appears in the engine's
dependency tree, and the engine's tests build and run on a machine with no
GTK installed at all.

It is not architecture for its own sake. It means the command line and the
application cannot disagree about what a format supports, because there is one
answer and both ask it. It means a format can be developed and tested without
a display. And it means the hard part — reading files nobody documented
properly — is testable in isolation from the part that has to be looked at.

## The shape of a conversion

Every format reads into one common representation and every format writes out
of it. Hub and spoke, not point to point: adding the fourth format added three
conversions rather than requiring three to be written.

```
   SL1 ─┐                        ┌─ SL1
   GOO ─┼─▶  PrintFile  +  ──────┼─ GOO
   CTB ─┤    LayerProvider       ├─ CTB
   PHZ ─┘                        └─ PHZ
```

`PrintFile` (`model.rs`) is the metadata: geometry, exposure, lift, per-layer
overrides, previews, and a bag of leftovers. Nearly every field is an
`Option`, because a format that does not record something must be
distinguishable from one that records zero. Units are in the field names —
`layer_height_mm`, `lift_speed_mm_min` — which has caught more mistakes than
it costs to type.

Layers are not in `PrintFile`. A print is thousands of images at tens of
megapixels; decoding them all to open a file would cost gigabytes and seconds.
A reader returns a `LayerProvider` instead, which decodes one layer when asked
(`layers.rs`). The interface wraps that in a small most-recently-used cache so
scrubbing the preview does not re-decode.

`convert.rs` plans a conversion before running it. The plan says what will be
lost — per-layer exposure a format cannot hold, previews it has no room for,
settings with no equivalent — computed by comparing what the file actually
contains against what the destination can store. Only real losses are
reported; saying "per-layer exposure will be lost" about a file that never had
any is noise.

## Reading files nobody wrote a specification for

These are proprietary formats, described by people who worked them out from
the outside. Three rules follow from that.

**Input is untrusted.** Every size and offset a file declares goes through
`limits.rs` before it is acted on. Offsets are checked against the file's real
length with arithmetic that cannot wrap; allocations are capped; layer counts
and resolutions have to be plausible. A malformed file produces a message a
person can act on, never a panic and never an unbounded allocation.

**Agreeing with yourself proves nothing.** A reader tested only against files
its own writer produced will happily agree with its own misunderstanding. That
is not a hypothetical: the GOO writer had a byte-order mistake in its grey
chunks that every internal test passed, and only a file from a real slicer
caught it. So each format is checked against files this project did not write —
from UVtools, from catibo, from PrusaSlicer — and the tests that do that skip
rather than fail when the file is not there, so the suite still runs on a bare
machine.

**When two implementations agree and a third does not, check the third.** CTB
writing was disabled for a while on the theory that it was broken, because
UVtools would not read what it produced. It turned out UVtools would not read
*catibo's* files either, and catibo is an implementation its author verified by
printing from it. The fault was a missing 84-byte record that Chitubox writes
and the others did not.

## Doing the work off the main thread

Converting is a per-layer pipeline — decode the source image, re-encode it for
the destination — and every layer is independent. The file is not: records
have to be written in order. `pipeline.rs` runs the expensive half on a pool
and hands the results back in index order to a single writer, which keeps the
append-only, atomic-rename property the conversion depends on.

How many layers are in flight comes from the machine, not a constant: core
count, capped by what a layer costs against what memory is actually free. A
11520x5120 panel is 59MB a layer, and thirty-two of those at once is 1.9GB on
a machine that may not have it. Workers are also held to a short lookahead, so
a fast decoder cannot queue a whole print into memory while the writer catches
up. A 438 layer print converts in eight seconds instead of forty-eight.

Files are written to a temporary name beside the destination and renamed only
when complete. A print file that stops halfway still opens, still reports a
layer count, and still looks finished in a file manager — and a half-written
file on a USB stick is something a person can carry to a printer.

## The interface

`shell.rs` is the window: a sidebar that folds to icons and a stack of pages.
`main.rs` is the pages. `viewer.rs` is the layer preview, `render.rs` the
downscaling behind it, `format_picker.rs` the format dropdown, `theme.rs` the
stylesheet, `penguin.rs` the thing that dances while a conversion runs.

Nothing here enumerates formats. The registry does, so a new format appears in
the input list, the output list, the file chooser's filters and the drop zone's
list without any of them being touched.

Two habits worth keeping:

**Errors carry suggestions.** `remedy.rs` turns a failure into things to try,
using what can be observed about the file — its size, whether it is on
removable media, whether the extension matches the contents. "The file could
not be read" on its own is a dead end.

**Layout is measured, not eyeballed.** Whether two controls line up, whether
the sidebar folds smoothly, whether anything is off the right-hand edge during
an animation — none of that is answerable by reading the code or looking at a
still. Three environment variables render or instrument the window so the
numbers can be read; see [TEST_PLAN.md](TEST_PLAN.md). Bugs found that way
that had survived being looked at: two controls 36 and 34 pixels tall, a
button eleven pixels off the centre line, and a fold that animated a value
nothing was drawing.

## Where the state lives

`settings.rs` and `history.rs` write plain text files under the user's config
directory, parsed leniently: an unknown key is ignored and a missing one takes
its default, so a file from a newer version does not break an older one. There
is no database and no schema migration, because there is not enough state to
justify either.
