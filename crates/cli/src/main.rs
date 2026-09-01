//! CheapAzSLA command line interface (§32).
//!
//! Every operation calls into cheapazsla-core. There is no conversion code in
//! this crate and there must never be: the point of the split is that the
//! command line and the desktop application cannot drift apart.
//!
//! Arguments are parsed by hand rather than with a crate. The surface is small
//! and stable, and it keeps the engine's dependency tree the only one that
//! matters.

use cheapazsla_core::remedy;
use cheapazsla_core::{convert, registry};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
CheapAzSLA - resin print file converter and inspector

USAGE
    cheapazsla <files...> [options]

OPTIONS
    -f, --format <id>       Output format, e.g. goo
        --input-format <id> Treat the input as this format instead of
                            detecting it, for when detection gets it wrong
    -o, --output <path>     Output file. Only valid with a single input
    -d, --output-dir <dir>  Directory to write into
        --overwrite         Replace an existing file instead of stopping
        --keep-both         Write alongside as \"name (1).ext\" if it exists
        --dry-run           Say what would happen, write nothing
    -i, --info              Print what is in each file and exit
        --validate          Check each file and report problems, then exit
        --formats           List supported formats and exit
    -v, --verbose           Report each layer as it is written
    -q, --quiet             Errors only
    -h, --help              This message
        --version           Version

EXAMPLES
    cheapazsla model.sl1 --format goo
    cheapazsla model.sl1 --output /media/usb/prints/model.goo
    cheapazsla *.sl1 --format goo --output-dir /media/usb/prints
    cheapazsla model.goo --info
";

#[derive(Default)]
struct Args {
    inputs: Vec<PathBuf>,
    format: Option<String>,
    /// Treat the input as this format rather than detecting it (§21).
    input_format: Option<String>,
    output: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    overwrite: bool,
    keep_both: bool,
    dry_run: bool,
    info: bool,
    validate: bool,
    formats: bool,
    verbose: bool,
    quiet: bool,
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS, // help or version
        Err(msg) => {
            eprintln!("cheapazsla: {msg}");
            eprintln!("Try 'cheapazsla --help'.");
            return ExitCode::from(2);
        }
    };

    if args.formats {
        list_formats();
        return ExitCode::SUCCESS;
    }
    if args.inputs.is_empty() {
        eprintln!("cheapazsla: no input files");
        eprintln!("Try 'cheapazsla --help'.");
        return ExitCode::from(2);
    }
    if args.output.is_some() && args.inputs.len() > 1 {
        eprintln!("cheapazsla: --output takes a single input; use --output-dir for several");
        return ExitCode::from(2);
    }

    let mut failures = 0usize;
    for input in &args.inputs {
        let outcome = if args.info {
            show_info(input, args.input_format.as_deref())
        } else if args.validate {
            validate(input)
        } else {
            convert_one(input, &args)
        };
        if let Err(msg) = outcome {
            eprintln!("cheapazsla: {}: {msg}", input.display());
            failures += 1;
            // §26: one bad file does not stop the rest of a batch.
        }
    }

    if failures > 0 {
        if args.inputs.len() > 1 && !args.quiet {
            eprintln!("cheapazsla: {} of {} failed", failures, args.inputs.len());
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "--version" => {
                println!("cheapazsla {}", cheapazsla_core::VERSION);
                return Ok(None);
            }
            "--formats" => a.formats = true,
            "-i" | "--info" => a.info = true,
            "--validate" => a.validate = true,
            "--overwrite" => a.overwrite = true,
            "--keep-both" => a.keep_both = true,
            "--dry-run" => a.dry_run = true,
            "-v" | "--verbose" => a.verbose = true,
            "-q" | "--quiet" => a.quiet = true,
            "-f" | "--format" => {
                a.format = Some(it.next().ok_or("--format needs a value")?);
            }
            "--input-format" => {
                a.input_format = Some(it.next().ok_or("--input-format needs a value")?);
            }
            "-o" | "--output" => {
                a.output = Some(PathBuf::from(it.next().ok_or("--output needs a path")?));
            }
            "-d" | "--output-dir" => {
                a.output_dir = Some(PathBuf::from(it.next().ok_or("--output-dir needs a path")?));
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option '{other}'"));
            }
            other => a.inputs.push(PathBuf::from(other)),
        }
    }
    if a.overwrite && a.keep_both {
        return Err("--overwrite and --keep-both contradict each other".into());
    }
    Ok(Some(a))
}

fn list_formats() {
    println!("ID       NAME                   READ   WRITE   EXTENSION");
    for h in registry::handlers() {
        let i = h.info();
        println!(
            "{:<8} {:<22} {:<6} {:<6}  .{}",
            i.id,
            i.name,
            if i.capabilities.reads { "yes" } else { "no" },
            if i.capabilities.writes { "yes" } else { "no" },
            i.extension
        );
    }
}

fn show_info(path: &Path, input_format: Option<&str>) -> Result<(), String> {
    let id = registry::identify(path).map_err(|e| explain(path, &e))?;
    let opened = match input_format {
        Some(id) => registry::open_as(path, id),
        None => registry::open(path),
    }
    .map_err(|e| explain(path, &e))?;
    let p = &opened.print;

    println!("{}", path.display());
    println!(
        "  format          {} ({:?} confidence)",
        id.detection.format_id, id.detection.confidence
    );
    println!("  detected by     {}", id.detection.reason);
    if id.extension_mismatch {
        println!("  WARNING         the extension disagrees with the contents");
    }
    println!(
        "  resolution      {} x {}",
        p.geometry.resolution_x, p.geometry.resolution_y
    );
    if let Some((x, y)) = p.geometry.pixel_size_um() {
        println!("  pixel size      {x:.2} x {y:.2} um");
    }
    println!("  layers          {}", p.layer_count());
    println!("  layer height    {} mm", p.exposure.layer_height_mm);
    if let Some(h) = p.height_mm() {
        println!("  print height    {h:.2} mm");
    }
    println!("  exposure        {} s", p.exposure.exposure_s);
    opt(
        "  bottom exposure",
        p.exposure.bottom_exposure_s.map(|v| format!("{v} s")),
    );
    opt(
        "  bottom layers  ",
        p.exposure.bottom_layers.map(|v| v.to_string()),
    );
    opt("  print time     ", p.print_time_s.map(fmt_time));
    opt(
        "  material       ",
        p.material_volume_ml.map(|v| format!("{v} ml")),
    );
    opt("  printer        ", p.machine_name.clone());
    println!("  thumbnails      {}", p.thumbnails.len());
    Ok(())
}

/// Print a value, or say plainly that the file does not record it (§13).
fn opt(label: &str, value: Option<String>) {
    println!("{label} {}", value.unwrap_or_else(|| "not recorded".into()));
}

fn fmt_time(s: u64) -> String {
    format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
}

fn validate(path: &Path) -> Result<(), String> {
    let id = registry::identify(path).map_err(|e| explain(path, &e))?;
    let handler = registry::by_id(id.detection.format_id).ok_or("no handler")?;
    let warnings = handler.validate(path).map_err(|e| explain(path, &e))?;
    if warnings.is_empty() {
        println!("{}: ok ({})", path.display(), id.detection.format_id);
    } else {
        println!("{}: {} warning(s)", path.display(), warnings.len());
        for w in warnings {
            println!("  {w}");
        }
    }
    Ok(())
}

fn convert_one(input: &Path, args: &Args) -> Result<(), String> {
    let format = args
        .format
        .clone()
        .or_else(|| {
            // An explicit --output implies its own format.
            args.output
                .as_ref()
                .and_then(|o| o.extension())
                .and_then(|e| e.to_str())
                .and_then(registry::by_extension)
                .map(|h| h.info().id.to_string())
        })
        .ok_or("no output format; pass --format or an --output with an extension")?;

    let destination = match &args.output {
        Some(o) => o.clone(),
        None => convert::destination_for(input, &format, args.output_dir.as_deref())
            .ok_or("could not work out an output path")?,
    };

    let plan = convert::plan_as(input, args.input_format.as_deref(), &format, &destination)
        .map_err(|e| explain(input, &e))?;

    if !args.quiet {
        for w in &plan.source_warnings {
            eprintln!("cheapazsla: {}: warning: {w}", input.display());
        }
        // §14: never drop information silently, even without a dialog to show.
        for l in &plan.losses {
            eprintln!(
                "cheapazsla: {}: dropping {} ({})",
                input.display(),
                l.what,
                l.because
            );
        }
    }

    let mut plan = plan;
    if plan.destination.exists() {
        if args.keep_both {
            plan.destination = convert::unique_path(&plan.destination);
        } else if !args.overwrite {
            return Err(format!(
                "{} already exists; pass --overwrite or --keep-both",
                plan.destination.display()
            ));
        }
    }

    if args.dry_run {
        println!(
            "would convert {} -> {} ({} layers, {} -> {})",
            input.display(),
            plan.destination.display(),
            plan.layer_count,
            plan.from.name,
            plan.to.name
        );
        return Ok(());
    }

    let started = std::time::Instant::now();
    let verbose = args.verbose && !args.quiet;
    let total = plan.layer_count;
    convert::run_with_progress(&plan, move |done, _| {
        if verbose {
            eprint!("\r  layer {done} of {total}");
        }
    })
    .map_err(|e| explain(input, &e))?;
    if verbose {
        eprintln!();
    }

    if !args.quiet {
        let size = std::fs::metadata(&plan.destination)
            .map(|m| m.len())
            .unwrap_or(0);
        println!(
            "{} -> {} ({} layers, {}, {:.1}s)",
            input.display(),
            plan.destination.display(),
            plan.layer_count,
            human_bytes(size),
            started.elapsed().as_secs_f32()
        );
    }
    Ok(())
}

/// The failure, then what to try, indented under it.
///
/// The suggestions come from the engine, so the command line says the same
/// things the interface does rather than keeping its own list.
fn explain(path: &Path, error: &cheapazsla_core::Error) -> String {
    let facts = remedy::FileFacts::observe(path);
    let suggestions = remedy::for_error(error, &facts);
    let mut out = error.to_string();
    if !suggestions.is_empty() {
        out.push_str("\n  what you can try:");
        for (i, s) in suggestions.iter().enumerate() {
            out.push_str(&format!("\n    {}. {}", i + 1, s.action));
            if !s.because.is_empty() {
                out.push_str(&format!("\n       {}", s.because));
            }
        }
    }
    out
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
