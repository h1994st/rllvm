//! Post-process a relocatable Mach-O object so it carries exactly one
//! `LC_SEGMENT_64` load command.
//!
//! `llvm-objcopy --add-section` on a Mach-O **object** appends a *second*
//! `LC_SEGMENT_64` to hold the new section. A relocatable Mach-O object is
//! supposed to carry exactly one segment load command, with an empty segname,
//! containing every section. Apple's `ld64` tolerates the extra segment, but
//! `ld64.lld` reads only the first segment and silently drops the section
//! (issue #203). This module folds the second segment into the first so the
//! resulting object is conforming.

/// `LC_SEGMENT_64` load-command constant.
const LC_SEGMENT_64: u32 = 0x19;
/// Size of the fixed part of an `LC_SEGMENT_64` command (everything before the
/// section table).
const SEG_HEADER_SIZE: usize = 72;
/// Size of one `section_64` entry.
const SECTION_SIZE: usize = 80;

struct Segment {
    /// Byte offset of the load command within the file.
    offset: usize,
    /// `cmdsize` field of the load command.
    cmdsize: usize,
    /// `nsects` field of the load command.
    nsects: u32,
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Walk the load-command list and collect every `LC_SEGMENT_64`.
fn find_segments(data: &[u8]) -> Option<Vec<Segment>> {
    if data.len() < 32 {
        return None;
    }
    if read_u32(data, 0) != 0xFEEDFACF {
        return None;
    }

    let ncmds = read_u32(data, 16) as usize;
    let mut offset = 32;
    let mut segments = Vec::new();

    for _ in 0..ncmds {
        if offset + 8 > data.len() {
            return None;
        }
        let cmd = read_u32(data, offset);
        let cmdsize = read_u32(data, offset + 4) as usize;
        if cmdsize < 8 {
            return None;
        }

        if cmd == LC_SEGMENT_64 {
            if offset + SEG_HEADER_SIZE > data.len() {
                return None;
            }
            segments.push(Segment {
                offset,
                cmdsize,
                nsects: read_u32(data, offset + 64),
            });
        }

        offset += cmdsize;
    }

    Some(segments)
}

/// Fold the second `LC_SEGMENT_64` (as appended by `llvm-objcopy
/// --add-section`) into the single segment a relocatable object is supposed
/// to carry, dropping the trailing command. Returns `None` when the layout is
/// not the expected two-segment form, leaving the caller to use the original
/// bytes.
pub fn fold_second_segment(bytes: &[u8]) -> Option<Vec<u8>> {
    let segments = find_segments(bytes)?;
    if segments.len() != 2 {
        return None;
    }

    let first = &segments[0];
    let second = &segments[1];

    // Only the one-section form produced by `llvm-objcopy --add-section` is
    // handled; anything else is left untouched.
    if second.nsects != 1 {
        return None;
    }
    // The appended segment must sit after the original one.
    if second.offset < first.offset + first.cmdsize {
        return None;
    }

    // Sanity-check the cmdsizes against the section counts.
    if first.cmdsize != SEG_HEADER_SIZE + (first.nsects as usize) * SECTION_SIZE {
        return None;
    }
    if second.cmdsize != SEG_HEADER_SIZE + SECTION_SIZE {
        return None;
    }

    let mut out = bytes.to_vec();

    // 1. Make room for one more section at the end of the first segment's
    //    section table.
    let insert_at = first.offset + first.cmdsize;
    out.splice(insert_at..insert_at, std::iter::repeat(0u8).take(SECTION_SIZE));

    // 2. Copy the second segment's section entry into the new slot,
    //    rewriting its segname to match the first segment's (empty for a
    //    relocatable object).
    let src = &bytes[second.offset + SEG_HEADER_SIZE..second.offset + SEG_HEADER_SIZE + SECTION_SIZE];
    let dst = insert_at;
    out[dst..dst + SECTION_SIZE].copy_from_slice(src);
    let first_segname = &bytes[first.offset + 8..first.offset + 24];
    out[dst + 16..dst + 32].copy_from_slice(first_segname);

    // 3. Bump the first segment's nsects and cmdsize.
    out[first.offset + 64..first.offset + 68]
        .copy_from_slice(&((first.nsects + 1).to_le_bytes()));
    out[first.offset + 4..first.offset + 8]
        .copy_from_slice(&((first.cmdsize + SECTION_SIZE) as u32).to_le_bytes());

    // 4. Remove the second segment's load command. Because the insertion in
    //    step 1 landed before the second segment, its offset is shifted by
    //    exactly SECTION_SIZE.
    let second_in_new = second.offset + SECTION_SIZE;
    out.splice(second_in_new..second_in_new + second.cmdsize, std::iter::empty());

    // 5. Update the Mach-O header: one fewer load command, and sizeofcmds
    //    shrinks by the second segment's size.
    out[16..20].copy_from_slice(&((read_u32(bytes, 16) - 1).to_le_bytes()));
    out[20..24]
        .copy_from_slice(&((read_u32(bytes, 20) as usize - second.cmdsize) as u32).to_le_bytes());

    Some(out)
}
