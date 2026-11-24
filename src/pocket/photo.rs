//! Photos for the Wall kit (pa14): the type from the file's first bytes, and the strip that runs on
//! arrival, before anything is written (plan section 8 rule 12). The strip is a hand-written walk
//! over the file's markers or chunks: segments that carry location, camera or other text are left
//! out and every other byte is copied as it came. Nothing is decoded and no pixel is re-encoded.
//!
//! - JPEG keeps APP0 `JFIF`, APP2 `ICC_PROFILE` and the image segments; it drops APP1 (Exif, XMP),
//!   APP13 (IPTC), COM, every other APPn and anything after the end marker (phones put extra
//!   pictures there, each with its own Exif).
//! - PNG drops `eXIf`, `tEXt`, `iTXt`, `zTXt` and `tIME`, and anything after `IEND`.
//! - WebP drops the `EXIF` and `XMP ` chunks, clears their two flags in `VP8X` and fixes the RIFF
//!   size.
//!
//! The Exif orientation is read before its segment goes and comes back as `rotate` (0, 90, 180 or
//! 270 degrees clockwise), so viewers turn the photo the way the camera held it.

/// A photo type the wall takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
    Webp,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Jpeg => "jpg",
            Kind::Png => "png",
            Kind::Webp => "webp",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Kind::Jpeg => "image/jpeg",
            Kind::Png => "image/png",
            Kind::Webp => "image/webp",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Kind> {
        match ext {
            "jpg" => Some(Kind::Jpeg),
            "png" => Some(Kind::Png),
            "webp" => Some(Kind::Webp),
            _ => None,
        }
    }
}

/// What the first bytes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    Photo(Kind),
    /// HEIC, HEIF or AVIF: a photo, but not one the wall shows (the page converts these first).
    Heic,
    Other,
}

/// The type from magic bytes only: a file name or a Content-Type is never trusted.
pub fn sniff(b: &[u8]) -> Sniff {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Sniff::Photo(Kind::Jpeg);
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Sniff::Photo(Kind::Png);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Sniff::Photo(Kind::Webp);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let brand = &b[8..12];
        if [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"avis"].iter().any(|x| brand == &x[..]) {
            return Sniff::Heic;
        }
    }
    Sniff::Other
}

/// A photo with its metadata taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    pub bytes: Vec<u8>,
    pub kind: Kind,
    /// Degrees clockwise a viewer turns it (from the Exif orientation; 0 without one).
    pub rotate: u16,
    pub w: Option<u32>,
    pub h: Option<u32>,
}

/// The file isn't a whole photo of its type (cut short, or not what its first bytes claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Broken;

/// Takes the metadata out of `b` (a photo of type `kind`, see [`sniff`]).
pub fn strip(kind: Kind, b: &[u8]) -> Result<Stripped, Broken> {
    match kind {
        Kind::Jpeg => strip_jpeg(b),
        Kind::Png => strip_png(b),
        Kind::Webp => strip_webp(b),
    }
}

/// Degrees clockwise for an Exif orientation. Mirrored ones (2, 4, 5, 7) keep their turn; the
/// mirror itself is lost, which nobody notices on a party photo.
fn degrees(orientation: u16) -> u16 {
    match orientation {
        3 | 4 => 180,
        5 | 6 => 90,
        7 | 8 => 270,
        _ => 0,
    }
}

/// The orientation tag (0x0112) in IFD0 of a TIFF block (Exif's body), as degrees. `None`
/// without one or when the block is malformed.
fn tiff_rotate(t: &[u8]) -> Option<u16> {
    let t = t.strip_prefix(b"Exif\0\0").unwrap_or(t);
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let s = t.get(o..o.checked_add(2)?)?;
        Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s = t.get(o..o.checked_add(4)?)?;
        let a = [s[0], s[1], s[2], s[3]];
        Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for k in 0..n.min(1024) {
        let e = ifd.checked_add(2 + k * 12)?;
        if u16_at(e)? == 0x0112 {
            return Some(degrees(u16_at(e + 8)?));
        }
    }
    None
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn strip_jpeg(b: &[u8]) -> Result<Stripped, Broken> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return Err(Broken);
    }
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    let (mut rotate, mut w, mut h) = (0, None, None);
    let (mut saw_scan, mut saw_end) = (false, false);
    let mut i = 2;
    while i < b.len() {
        if b[i] != 0xFF {
            return Err(Broken);
        }
        // Fill bytes before a marker
        while i < b.len() && b[i] == 0xFF {
            i += 1;
        }
        let m = *b.get(i).ok_or(Broken)?;
        i += 1;
        match m {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                saw_end = true;
                // Whatever follows the end marker (a second picture, a maker's trailer) stays out
                break;
            }
            0x01 | 0xD0..=0xD7 => {
                out.extend_from_slice(&[0xFF, m]);
                continue;
            }
            0x00 | 0xD8 => return Err(Broken),
            _ => {}
        }
        let len = be16(b, i).ok_or(Broken)? as usize;
        if len < 2 || i + len > b.len() {
            return Err(Broken);
        }
        let body = &b[i + 2..i + len];
        let keep = match m {
            0xE0 => body.starts_with(b"JFIF\0"),
            0xE1 => {
                if body.starts_with(b"Exif\0\0") {
                    if let Some(r) = tiff_rotate(body) {
                        rotate = r;
                    }
                }
                false
            }
            0xE2 => body.starts_with(b"ICC_PROFILE\0"),
            0xE3..=0xEF | 0xFE => false,
            // Start of frame (not DHT C4, JPG C8 or DAC CC): the size
            0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                if body.len() >= 5 {
                    h = Some(u32::from(be16(body, 1).ok_or(Broken)?));
                    w = Some(u32::from(be16(body, 3).ok_or(Broken)?));
                }
                true
            }
            _ => true,
        };
        if keep {
            out.extend_from_slice(&[0xFF, m]);
            out.extend_from_slice(&b[i..i + len]);
        }
        i += len;
        if m == 0xDA {
            saw_scan = true;
            // The scan's data runs to the next marker that is neither a stuffed 0xFF nor a restart
            let start = i;
            loop {
                let (x, y) = (*b.get(i).ok_or(Broken)?, *b.get(i + 1).ok_or(Broken)?);
                if x == 0xFF && y != 0x00 && !(0xD0..=0xD7).contains(&y) && y != 0xFF {
                    break;
                }
                i += if x == 0xFF && y != 0xFF { 2 } else { 1 };
            }
            out.extend_from_slice(&b[start..i]);
        }
    }
    if !saw_scan || !saw_end {
        return Err(Broken);
    }
    Ok(Stripped { bytes: out, kind: Kind::Jpeg, rotate, w, h })
}

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn strip_png(b: &[u8]) -> Result<Stripped, Broken> {
    if !b.starts_with(PNG_SIG) {
        return Err(Broken);
    }
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(PNG_SIG);
    let (mut rotate, mut w, mut h) = (0, None, None);
    let mut i = PNG_SIG.len();
    let mut first = true;
    loop {
        let head = b.get(i..i + 8).ok_or(Broken)?;
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let ty = &head[4..8];
        let end = i.checked_add(12).and_then(|x| x.checked_add(len)).ok_or(Broken)?;
        if end > b.len() {
            return Err(Broken);
        }
        let data = &b[i + 8..i + 8 + len];
        if first && (ty != b"IHDR" || len < 8) {
            return Err(Broken);
        }
        first = false;
        let keep = match ty {
            b"IHDR" => {
                w = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
                h = Some(u32::from_be_bytes([data[4], data[5], data[6], data[7]]));
                true
            }
            b"eXIf" => {
                if let Some(r) = tiff_rotate(data) {
                    rotate = r;
                }
                false
            }
            b"tEXt" | b"iTXt" | b"zTXt" | b"tIME" => false,
            _ => true,
        };
        if keep {
            out.extend_from_slice(&b[i..end]);
        }
        i = end;
        if ty == b"IEND" {
            break;
        }
    }
    Ok(Stripped { bytes: out, kind: Kind::Png, rotate, w, h })
}

fn le24(b: &[u8]) -> u32 {
    u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16)
}

/// VP8X flags: bit 3 says there is an EXIF chunk, bit 2 an XMP chunk.
const VP8X_EXIF: u8 = 0x08;
const VP8X_XMP: u8 = 0x04;

fn strip_webp(b: &[u8]) -> Result<Stripped, Broken> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return Err(Broken);
    }
    let riff = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let end = riff.checked_add(8).ok_or(Broken)?;
    if end > b.len() || riff < 4 {
        return Err(Broken);
    }
    let mut out = Vec::with_capacity(end);
    out.extend_from_slice(b"RIFF\0\0\0\0WEBP");
    let (mut rotate, mut w, mut h) = (0, None, None);
    let mut flags_at = None;
    let mut image = false;
    let mut i = 12;
    while i < end {
        let head = b.get(i..i + 8).filter(|_| i + 8 <= end).ok_or(Broken)?;
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let data_end = (i + 8).checked_add(size).ok_or(Broken)?;
        if data_end > end {
            return Err(Broken);
        }
        let data = &b[i + 8..data_end];
        // Chunks are padded to an even size; a last chunk may end without its pad byte
        let next = (data_end + (size & 1)).min(end);
        let keep = match &head[..4] {
            b"EXIF" => {
                if let Some(r) = tiff_rotate(data) {
                    rotate = r;
                }
                false
            }
            b"XMP " => false,
            b"VP8X" => {
                if size >= 10 {
                    w = Some(le24(&data[4..7]) + 1);
                    h = Some(le24(&data[7..10]) + 1);
                    flags_at = Some(out.len() + 8);
                }
                image = true;
                true
            }
            b"VP8 " => {
                if w.is_none() && size >= 10 && data[3..6] == [0x9D, 0x01, 0x2A] {
                    w = Some(u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3FFF));
                    h = Some(u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3FFF));
                }
                image = true;
                true
            }
            b"VP8L" => {
                if w.is_none() && size >= 5 && data[0] == 0x2F {
                    let bits = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                    w = Some((bits & 0x3FFF) + 1);
                    h = Some(((bits >> 14) & 0x3FFF) + 1);
                }
                image = true;
                true
            }
            _ => true,
        };
        if keep {
            out.extend_from_slice(&b[i..next]);
        }
        i = next;
    }
    if !image {
        return Err(Broken);
    }
    if let Some(at) = flags_at {
        out[at] &= !(VP8X_EXIF | VP8X_XMP);
    }
    let size = u32::try_from(out.len() - 8).map_err(|_| Broken)?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(Stripped { bytes: out, kind: Kind::Webp, rotate, w, h })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_bytes_only() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Sniff::Photo(Kind::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Sniff::Photo(Kind::Png));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Sniff::Photo(Kind::Webp));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0"), Sniff::Heic);
        assert_eq!(sniff(b"GIF89a......"), Sniff::Other);
        assert_eq!(sniff(b""), Sniff::Other);
    }

    #[test]
    fn a_cut_short_jpeg_is_broken() {
        assert_eq!(strip(Kind::Jpeg, &[0xFF, 0xD8, 0xFF, 0xE1, 0x00]), Err(Broken));
        assert_eq!(strip(Kind::Jpeg, &[0xFF, 0xD8, 0xFF, 0xD9]), Err(Broken), "no scan");
        assert_eq!(strip(Kind::Png, b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"), Err(Broken));
        assert_eq!(strip(Kind::Webp, b"RIFF\xff\0\0\0WEBP"), Err(Broken));
    }
}
