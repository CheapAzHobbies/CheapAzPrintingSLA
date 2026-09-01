# Test plan

Two halves. The automated suite runs on every change and covers the engine.
The manual checklist covers the interface, which cannot be asserted from a
test harness.

A converter that only reads files it wrote itself proves very little, so the
automated tests are built to run against genuine slicer output as well as
synthetic fixtures.

## Running the automated tests

```bash
cargo test
```

Tests that need real files are skipped unless you point at them:

```bash
CHEAPAZSLA_REAL_SL1=/path/to/real.sl1 \
CHEAPAZSLA_REAL_GOO=/path/to/real.goo \
cargo test
```

Point both at the same model in the two formats where you can. Several tests
compare them layer by layer, which is the strongest check available: two
independent code paths must produce the same image.

## Automated coverage

### Data model

| Check | Test |
|---|---|
| Pixel size derived from display size and resolution | `pixel_size_is_derived_from_display_and_resolution` |
| Missing display size yields no pixel size, not a guess | `pixel_size_is_absent_when_display_size_is_unknown` |
| Zero resolution does not divide by zero | `pixel_size_does_not_divide_by_zero` |
| Bottom layers use bottom exposure | `bottom_layers_use_bottom_exposure` |
| A per-layer override beats the bottom rule | `a_per_layer_override_wins_over_bottom_rules` |
| Exposure past the end is absent | `exposure_of_a_layer_past_the_end_is_none` |
| Height comes from the top layer | `height_comes_from_the_top_layer` |
| Blank and exposed pixel counting | `blank_layers_report_themselves_as_blank`, `exposed_pixels_respects_the_threshold` |

### Untrusted input

| Check | Test |
|---|---|
| Range inside the file accepted | `a_range_inside_the_file_is_accepted` |
| Range past the end rejected | `a_range_past_the_end_is_rejected` |
| Offset plus length that would overflow rejected | `an_offset_and_length_that_would_overflow_are_rejected` |
| Oversized allocation refused | `an_oversized_allocation_is_refused` |
| Zero and absurd resolutions refused | `zero_and_absurd_resolutions_are_refused` |
| Absurd layer count refused | `an_absurd_layer_count_is_refused` |
| Empty file not identified | `an_empty_file_is_not_identified_as_anything` |
| Random bytes named .sl1 rejected | `random_bytes_named_sl1_are_rejected` |
| Truncated archive low confidence, fails to open | `a_truncated_zip_is_low_confidence_and_fails_to_open` |
| Truncated GOO errors rather than panics | `truncating_a_goo_is_an_error_not_a_panic` |
| Zip-bomb shaped entry capped | `a_zip_bomb_style_entry_is_capped_by_the_allocation_limit` |
| Layer that is not a PNG fails cleanly | `a_layer_that_is_not_a_png_fails_to_decode_cleanly` |

### Lazy layers

| Check | Test |
|---|---|
| Layers fetched by index | `layers_can_be_fetched_by_index` |
| Index past the end errors, not panics | `fetching_a_layer_past_the_end_is_an_error_not_a_panic` |
| Cache evicts oldest, stays correct | `the_cache_holds_recent_layers_and_evicts_the_oldest` |
| Cache clears | `clearing_the_cache_empties_it` |

### Format detection

| Check | Test |
|---|---|
| Real SL1 detected with high confidence | `detects_a_real_sl1_with_high_confidence` |
| Contents beat a wrong extension | `an_sl1_named_with_the_wrong_extension_is_identified_by_content` |
| Declared layer count mismatch warns, does not fail | `a_declared_layer_count_that_disagrees_produces_a_warning_not_a_failure` |

### SL1

| Check | Test |
|---|---|
| Metadata matches the file's own config.ini | `reads_metadata_from_a_real_sl1` |
| Layer bitmaps at the declared size | `decodes_real_layer_bitmaps_at_the_declared_size` |
| Missing layer height reported by name | `a_missing_layer_height_is_reported_by_name` |
| Negative layer height refused | `a_negative_layer_height_is_refused` |
| Round trip preserves layers and settings | `an_sl1_survives_a_round_trip_through_our_own_writer` |

### CTB

| What | Test |
|---|---|
| Detection by magic and version | `ctb.rs` |
| Extension alone is not enough | `ctb.rs` |
| Metadata survives a round trip | `ctb.rs` |
| Layers decode to the right pixels | `ctb.rs` |
| Run-length codec, every length class | `ctb_rle.rs` |
| Truncated, over-large, zero-resolution files | `ctb.rs` |
| Layers pointing outside the file | `ctb.rs` |
| Encrypted files refused, not misread | `ctb.rs` |
| CTB to GOO, pixel for pixel | `ctb.rs` |
| **A file Chitubox produced** | `ctb.rs`, skipped without `CHEAPAZSLA_REAL_CTB` |

Every row but the last reads a file this project built, so they prove the
reader agrees with itself rather than with Chitubox. The last row is the one
that decides whether CTB works.

### GOO

| Check | Test |
|---|---|
| Our decoder reproduces a real GOO layer | `our_decoder_reproduces_a_real_goo_layer` |
| A GOO layer matches the same layer from the SL1 | `a_goo_layer_matches_the_same_layer_from_the_sl1` |
| Encode then decode is lossless | `round_trips_pixels_through_the_encoder` |
| Long runs split across chunk sizes correctly | `a_long_run_is_split_across_chunks_correctly` |
| Real GOO from other software reads | `reads_a_real_goo_written_by_other_software` |
| Our own GOO reads back identically | `a_goo_we_wrote_reads_back_identically` |

### Conversion

| Check | Test |
|---|---|
| SL1 to GOO produces a structurally valid file | `converts_a_real_sl1_into_a_goo` |
| Every layer identical after conversion | `every_layer_survives_the_conversion_pixel_for_pixel` |
| One progress report per layer | `conversion_reports_progress_for_every_layer` |

### Honesty

| Check | Test |
|---|---|
| Anything claiming to write can write | `every_format_claiming_to_write_can_actually_write` |
| Anything claiming to read rejects nonsense | `every_format_claiming_to_read_has_a_working_opener` |

These two exist because a format once advertised a writer it did not have, and
the interface built its output list from that claim.

### Interface

Run with `cargo test --workspace --bins`. The interface is a binary crate, so
`--lib` finds nothing and passes without running a thing.

| Check | Test |
|---|---|
| Square pixels are left alone | `square_pixels_are_left_alone` |
| A taller pixel makes a taller image | `a_taller_pixel_makes_a_taller_image` |
| Corrected preview matches the panel's physical proportions | `correcting_gets_the_physical_proportions_right` |
| Resampling keeps hard edges, inventing no grey | `resampling_keeps_hard_edges` |
| The save indicator sheet slices into every frame | `the_sheet_slices_into_every_frame` |
| Its frames are not all transparent | `frames_are_not_all_empty` |

### Settings

| Check | Test |
|---|---|
| Warns before dropping information by default | `defaults_warn_before_dropping_information` |
| Recent folders most recent first, no duplicates | `recent_dirs_are_most_recent_first_without_duplicates` |
| Recent folder list is capped | `recent_dirs_are_capped` |
| Open dialog prefers default then last used | `the_open_dialog_prefers_the_chosen_default_then_the_last_used` |
| Pinning is idempotent and reversible | `pinning_a_drive_is_idempotent_and_removable` |
| A corrupt settings file does not stop startup | `a_corrupt_settings_file_does_not_stop_startup` |

## Manual checklist

Run through this before a release. Tick each line.

### Opening

- [ ] Launch with no arguments: the drop target appears in the viewer pane
- [ ] `Browse Files…` opens the native picker
- [ ] The picker filters to supported files, and `All Files` is available
- [ ] Open an SL1: metadata fills in, first layer displays
- [ ] Open a GOO: same
- [ ] Drag a file from the file manager onto the window: it loads
- [ ] Drag a file from a USB drive: it loads
- [ ] Open a file renamed to the wrong extension: it is identified by content
      and a warning says the extension disagrees
- [ ] Open a text file renamed to `.sl1`: a clear error, no crash
- [ ] `Ctrl+O` opens the picker

### Inspecting

- [ ] Values shown match what the file contains
- [ ] Anything the file does not record reads "not recorded", never 0
- [ ] Layer slider scrubs, the image follows
- [ ] First, previous, next, last all work
- [ ] Play cycles layers, pause stops it
- [ ] `Space`, arrows, `Home`, `End` do the same
- [ ] The label reports the downscale factor on large layers
- [ ] On a printer with non-square pixels the preview says it has been
      corrected, and the part is not stretched
- [ ] The preview image fills its pane rather than sitting in a corner
- [ ] Zoom in and out with the buttons, the wheel, and + and -
- [ ] The point under the pointer stays put when zooming with the wheel
- [ ] Click and drag pans once zoomed in, and does nothing while fit
- [ ] Fit (0) returns to the whole plate, Actual size (1) shows one to one
- [ ] The zoom stays put when stepping to the next layer
- [ ] Scrollbars appear only when zoomed past the pane
- [ ] Scrubbing quickly does not freeze the window
- [ ] Scrubbing fast keeps the picture moving rather than holding one frame
- [ ] While it is showing a stand-in, the caption says so and the layer number
      is dimmed, and it never claims to be the layer that was selected
- [ ] Stopping resolves to the exact layer within a moment

### Converting

- [ ] Output format lists only formats that can actually be written
- [ ] `Save as` is pre-filled from the source name with the extension swapped
- [ ] Editing the name is respected
- [ ] A name typed without an extension gets the right one
- [ ] A name containing a slash is refused with a message
- [ ] `Save to` defaults to beside the original
- [ ] `Choose…` opens the native folder picker
- [ ] Free space is shown for the chosen folder
- [ ] Converting produces a file that opens again in CheapAzSLA
- [ ] Converting produces a file another tool accepts
- [ ] Progress shows layer, percentage and an estimate, and fills the bar
- [ ] The window stays responsive during a conversion
- [ ] The completion toast reports size and time, and `Open Folder` works
- [ ] `Ctrl+Enter` converts

### Warnings and errors

- [ ] Converting to a format that cannot hold everything lists what is dropped
- [ ] `Do not ask me again` suppresses it afterwards
- [ ] Ticking that box then cancelling does **not** suppress it
- [ ] Settings can turn the warning back on
- [ ] Converting onto an existing file offers Replace, Keep Both, Cancel
- [ ] Keep Both writes `name (1).goo`
- [ ] Choosing a folder with no write permission gives a clear message
- [ ] Unplugging the destination drive before converting gives a clear message,
      and nothing is written anywhere else

### Drives

- [ ] Settings lists mounted drives with free space
- [ ] Pinning a drive adds a shortcut button
- [ ] The shortcut sets the destination
- [ ] A subfolder is created if it does not exist
- [ ] Unplugging a pinned drive greys the button rather than removing it
- [ ] Plugging a drive in updates the list without a restart

### Clearing and settings

- [ ] Clear returns the viewer to the drop target
- [ ] Clear keeps the format, destination and pinned drives
- [ ] `Ctrl+W` clears
- [ ] The default open folder is honoured next time
- [ ] Settings survive a restart

### Window

- [ ] Minimise, maximise and close are present and work
- [ ] Double-clicking the header bar maximises
- [ ] The window can be dragged by the header bar

### Window sizes

- [ ] Tiles to half the screen without the layout breaking
- [ ] Tiles to a quarter of the screen
- [ ] Narrowing drops the information panel beside the preview
- [ ] Narrowing further leaves the sidebar as icons, still navigable
- [ ] Nothing overlaps at the smallest size the window will take
- [ ] The sidebar folds and unfolds smoothly while the window is dragged, not
      only when it is resized in one jump, and the two directions look alike

Two environment variables help with the last two, because neither can be
judged from a still and both have been wrong in ways that looked right when
triggered any other way:

    CHEAPAZSLA_DEBUG_SIZE=1   what is holding the window's minimum width open
    CHEAPAZSLA_DEBUG_FOLD=1   the sidebar's drawn width, step by step, while
                              the window is walked in and back out

For the fold, the numbers should descend and climb evenly. A repeated value is
a stall, a large gap is a jump, and the two directions should read as reverses
of each other. It walks the window in three ways — slowly, faster than the
fold settles, and back and forth across the step every few frames — because
the fold has been wrong in a different way under each. In the last of those
the rail will not reach either end, which is correct; what matters is that it
changes direction on the next frame rather than carrying on the old way. libadwaita also warns on stderr when the content is wider than
the window it was given, which is the fastest signal that a minimum has
regressed.

### Appearance

- [ ] Dark by default
- [ ] Resizing keeps the layout sensible
- [ ] Tab reaches every control, focus is visible
- [ ] Readable at 150% and 200% scaling

## Adding to this

A new format is not "supported" until it has entries in the automated tables
above: detection, malformed input, metadata against ground truth, layer
dimensions, and a round trip. See [ADDING_A_FORMAT.md](ADDING_A_FORMAT.md).
