# Test fixtures

## catibo-plain.ctb, catibo-encrypted.ctb

Two CTB files, 64x32, four layers each, one with the layer data in the clear
and one obfuscated with key `0x12345678`.

They were not written by this project. They were written by
[catibo](https://github.com/cbiffle/catibo), an independent reverse
engineering of the format whose author verified it by printing from it on a
Creality LD-002R. That is what makes them worth having: every other CTB test
here reads a file CheapAzSLA built, and so can only show that the reader
agrees with itself. These show whether it agrees with somebody else.

Regenerated with `tools/make_ctb_fixtures.sh`, which clones catibo, builds it
and asks it for these files. The layer patterns are fixed there, and
`ctb_reference.rs` checks the pixels that come back against them.

catibo is BSD-2-Clause, Copyright (c) 2020 Cliff L. Biffle. These are files its
encoder produced rather than any part of its source.
