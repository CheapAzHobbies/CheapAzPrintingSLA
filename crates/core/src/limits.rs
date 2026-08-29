//! Safety limits for untrusted input (§42).
//!
//! Print files are data from elsewhere. A malformed or hostile file must not
//! be able to make CheapAzSLA allocate unbounded memory or read outside the
//! data it was given. Every parser routes size decisions through here.

use crate::error::{FormatError, Result};

/// Largest layer bitmap accepted, in pixels. 16384x16384 covers every panel
/// on the market with headroom, and caps a single layer at 256 MB.
pub const MAX_LAYER_PIXELS: u64 = 16384 * 16384;

/// Largest single allocation a parser will make from a length field.
pub const MAX_ALLOCATION: u64 = 512 * 1024 * 1024;

/// Largest layer count accepted. At 10 um layers this is 10 metres of travel.
pub const MAX_LAYERS: u32 = 1_000_000;

/// Largest thumbnail accepted, in pixels.
pub const MAX_THUMBNAIL_PIXELS: u64 = 4096 * 4096;

/// Check that a length taken from a file is safe to allocate.
pub fn check_allocation(declared: u64) -> Result<usize> {
    if declared > MAX_ALLOCATION {
        return Err(FormatError::AllocationTooLarge {
            declared,
            limit: MAX_ALLOCATION,
        }
        .into());
    }
    Ok(declared as usize)
}

/// Check that `offset .. offset + length` lies inside a file of `file_size`.
///
/// Uses checked arithmetic: a crafted file can otherwise pick values whose sum
/// wraps and appears to be in bounds.
pub fn check_range(offset: u64, length: u64, file_size: u64) -> Result<()> {
    let end = offset
        .checked_add(length)
        .ok_or(FormatError::OffsetOutOfBounds {
            offset,
            length,
            file_size,
        })?;
    if end > file_size {
        return Err(FormatError::OffsetOutOfBounds {
            offset,
            length,
            file_size,
        }
        .into());
    }
    Ok(())
}

/// Check that a resolution is plausible and will not overflow when multiplied.
pub fn check_resolution(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(FormatError::InvalidValue {
            field: "resolution".into(),
            value: format!("{width}x{height}"),
            reason: "must be at least one pixel in each direction".into(),
        }
        .into());
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_LAYER_PIXELS {
        return Err(FormatError::InvalidValue {
            field: "resolution".into(),
            value: format!("{width}x{height}"),
            reason: format!("exceeds the {MAX_LAYER_PIXELS} pixel safety limit"),
        }
        .into());
    }
    Ok(())
}

/// Check that a declared layer count is plausible.
pub fn check_layer_count(count: u64) -> Result<u32> {
    if count > MAX_LAYERS as u64 {
        return Err(FormatError::InvalidValue {
            field: "layer count".into(),
            value: count.to_string(),
            reason: format!("exceeds the {MAX_LAYERS} layer safety limit"),
        }
        .into());
    }
    Ok(count as u32)
}
