# Adding a format

The most important developer task in this project (§45). It touches one crate
and requires no interface work.

## 1. Create the handler

Add `crates/core/src/formats/<id>.rs` and implement `FormatHandler`:

```rust
pub struct MyHandler;

static INFO: FormatInfo = FormatInfo {
    id: "myfmt",
    name: "My Printer Format",
    extension: "myf",
    aliases: &[],
    description: "One or two sentences shown in the format info popover.",
    limitations: &["What it cannot store, in plain language"],
    capabilities: Capabilities { /* be honest here */ ..Capabilities::minimal() },
};

impl FormatHandler for MyHandler {
    fn info(&self) -> &'static FormatInfo { &INFO }
    fn detect(&self, path: &Path, data: &[u8]) -> Detection { ... }
    fn validate(&self, path: &Path) -> Result<Vec<String>> { ... }
    fn open(&self, path: &Path) -> Result<OpenedFile> { ... }
    fn write(&self, path: &Path, print: &PrintFile, layers: &dyn LayerProvider) -> Result<()> { ... }
}
```

`capabilities` is not decoration. It drives the warning shown before a
conversion drops information, so claiming a capability the format lacks
causes silent data loss.

## 2. Register it

One line in `crates/core/src/registry.rs`:

```rust
static MYFMT: MyHandler = MyHandler;
vec![&SL1, &MYFMT]
```

That is the whole integration. The interface and the command line both read
this list, so the format appears in both with no further change.

## 3. Rules for the parser

- Take every size and offset through `limits::` — never trust a length field.
- Return `None` for anything the file does not record. Never substitute a
  default and present it as fact.
- Normalise units on read: millimetres and seconds, as named in the model.
- Put values you cannot map into `PrintFile::extra`, so conversions can report
  what they are dropping.

## 4. Tests

Required before the format is listed as supported:

- Detection, including a file with a deliberately wrong extension
- A truncated file, a file of random bytes, and an empty file
- An out-of-range offset and an absurd declared length
- Metadata values checked against ground truth from a real file
- Layer dimensions and a layer index past the end
- A round trip through the common model, checking that print-meaningful
  values survive

Byte-identical round trips are not the goal. Surviving print information is.
