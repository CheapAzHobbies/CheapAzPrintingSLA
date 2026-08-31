# Assets

## icon-source.png

The application icon: a Benchy on a stack of cured layers, in the same
desaturated teal the interface uses as its accent.

Generated artwork, then cut out: the original had a grey gradient baked in
around the rounded square, which reads as a sticker on a card once it is
sitting in an application grid. `icon-source.png` is the rounded square alone
with the surround transparent, at 832x832.

`icons/` holds it resized to the sizes the hicolor theme is looked up at.
Regenerate them from the source rather than editing them individually.

It stays legible down to 32 pixels. At 16 the Benchy is mush but the layer
stack silhouette still reads, which is the part that matters at that size.

## penguin_saving.png

The save indicator. See the credits section of the top-level README: it is the
Club Penguin dance recoloured to a silhouette, carried over from
[lens](https://github.com/CheapAzHobbies/lens), and used as a progress
indicator rather than shipped as artwork.

The white sticker outline around it is generated, not hand-drawn: run
`tools/outline_penguin.py assets/penguin_saving.png`. It dilates each frame's
alpha, rounds the corners off and fills the result white behind the original,
one grid cell at a time so an outline cannot bleed into a neighbouring frame.
The first run keeps the un-outlined sheet as `penguin_saving.png.orig` and
every later run starts from that, so the border is never applied twice.
