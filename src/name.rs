//! Names: the 8.3 field, the long name, and the journey between them.
//!
//! FAT stores two names for a file. The **short name** is eleven bytes — eight
//! of base and three of extension, space-padded, uppercase — and it is the one
//! every driver can read. The **long name** is a chain of extra directory
//! slots, each carrying thirteen UTF-16 code units, that a driver which
//! understands them assembles into the real name.
//!
//! A file needs long-name slots when its name will not fit the short field
//! *exactly*: too long, mixed case, a character the short field forbids, more
//! than one dot. When it does fit, the short entry alone is written — which is
//! what every other implementation does and what keeps a simple directory
//! simple.
//!
//! Case is the subtle part. `README.TXT` and `readme.txt` both fit 8.3, and the
//! second is stored as the first with the two Windows NT case flags set rather
//! than with a long name. `ReadMe.txt` does not fit either way, and gets one.

use mkfs_dos::structs::dirent::{DirEntry, LfnEntry, NAME_LEN};

use crate::error::{Error, Result};

/// Bit 3 of the `lcase` byte: the base name is really lowercase.
pub const LCASE_BASE: u8 = 0x08;
/// Bit 4: the extension is really lowercase.
pub const LCASE_EXT: u8 = 0x10;

/// A short name, and the case flags that go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortName {
    /// The eleven raw bytes, space-padded.
    pub raw: [u8; NAME_LEN],
    /// The NT case flags.
    pub lcase: u8,
}

impl ShortName {
    /// The name as text, case flags applied.
    pub fn to_string_lossy(self) -> String {
        let entry = DirEntry {
            name: self.raw,
            lcase: self.lcase,
            ..Default::default()
        };
        entry.short_name()
    }
}

/// Is this byte allowed in a short name?
///
/// The forbidden set is the specification's, plus everything below 0x20 — and
/// space, which is padding and cannot be content. Bytes above 0x7f are legal on
/// disk but code-page dependent, so a name that needs them gets a long name
/// instead of a guess about which code page the reader will use.
pub fn is_valid_short_byte(b: u8) -> bool {
    b > 0x20 && b < 0x7f && !b"\"*+,./:;<=>?[\\]|".contains(&b)
}

/// Is this character allowed in a long name?
pub fn is_valid_long_char(c: char) -> bool {
    !matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|') && c >= ' '
}

/// Check a name is one FAT can store at all, long or short.
pub fn validate(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_name(name, "a name cannot be empty"));
    }
    if name == "." || name == ".." {
        return Err(Error::invalid_name(name, "'.' and '..' are not names"));
    }
    if name.chars().count() > 255 {
        return Err(Error::invalid_name(
            name,
            "a long name holds at most 255 characters",
        ));
    }
    if let Some(c) = name.chars().find(|&c| !is_valid_long_char(c)) {
        return Err(Error::invalid_name(name, format!("'{c}' is not allowed")));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(Error::invalid_name(
            name,
            "a name cannot end in a space or a dot",
        ));
    }
    Ok(())
}

/// Split a name into base and extension at the last dot.
///
/// A leading dot is part of the base, not an extension: `.config` is a base of
/// `.config` and no extension — which is why it needs a long name, the short
/// field having no way to begin with a dot.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

/// The short name a name maps to exactly, if it does.
///
/// `None` means a long name is needed. That is the whole decision: everything
/// else follows from it.
pub fn exact_short_name(name: &str) -> Option<ShortName> {
    let (base, ext) = split_ext(name);
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    if name.matches('.').count() > 1 {
        return None;
    }
    if !base.bytes().chain(ext.bytes()).all(is_valid_short_byte) {
        return None;
    }

    // Each half must be all one case, since there is one flag for each and no
    // way to say "the third letter is lowercase".
    let base_case = uniform_case(base)?;
    let ext_case = uniform_case(ext)?;

    let mut raw = [b' '; NAME_LEN];
    raw[..base.len()].copy_from_slice(base.to_ascii_uppercase().as_bytes());
    raw[8..8 + ext.len()].copy_from_slice(ext.to_ascii_uppercase().as_bytes());

    let mut lcase = 0;
    if base_case == Case::Lower {
        lcase |= LCASE_BASE;
    }
    if ext_case == Case::Lower {
        lcase |= LCASE_EXT;
    }
    Some(ShortName { raw, lcase })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Upper,
    Lower,
}

/// The case of a string, or `None` when it is mixed.
fn uniform_case(s: &str) -> Option<Case> {
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    match (has_upper, has_lower) {
        (true, true) => None,
        (_, true) => Some(Case::Lower),
        _ => Some(Case::Upper),
    }
}

/// Build a short name for a long one, avoiding the names `taken` reports.
///
/// The basis is the name with everything the short field forbids removed and
/// the rest uppercased; if that collides or the name was lossy, a `~1` tail is
/// appended and the number climbs. Windows does the same, and the numbering
/// being visible is why `Program Files` is `PROGRA~1`.
pub fn generate_short_name(name: &str, taken: impl Fn(&[u8; NAME_LEN]) -> bool) -> Result<ShortName> {
    let (base, ext) = split_ext(name);

    let basis: Vec<u8> = base
        .chars()
        .filter(|&c| c != ' ' && c != '.')
        .map(|c| {
            let b = c as u32;
            if b < 128 && is_valid_short_byte(b as u8) {
                (b as u8).to_ascii_uppercase()
            } else {
                b'_'
            }
        })
        .collect();
    let ext: Vec<u8> = ext
        .chars()
        .take(3)
        .map(|c| {
            let b = c as u32;
            if b < 128 && is_valid_short_byte(b as u8) {
                (b as u8).to_ascii_uppercase()
            } else {
                b'_'
            }
        })
        .collect();

    let basis = if basis.is_empty() { b"_".to_vec() } else { basis };

    for n in 1..=999_999u32 {
        let tail = format!("~{n}");
        let keep = 8usize.saturating_sub(tail.len()).min(basis.len());
        let mut raw = [b' '; NAME_LEN];
        raw[..keep].copy_from_slice(&basis[..keep]);
        raw[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        raw[8..8 + ext.len()].copy_from_slice(&ext);
        if !taken(&raw) {
            return Ok(ShortName { raw, lcase: 0 });
        }
    }
    Err(Error::invalid_name(
        name,
        "no short name is free — a million names in one directory already collide",
    ))
}

/// The long-name slots for `name`, in the order they appear on disk.
///
/// Which is backwards: the fragment holding the *end* of the name comes first
/// and carries the `0x40` flag. A reader walking a directory forwards therefore
/// meets the tail of the name before its head, and assembles in reverse.
pub fn encode_lfn(name: &str, checksum: u8) -> Vec<LfnEntry> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let fragments = units.len().div_ceil(LfnEntry::CHARS).max(1);

    let mut out = Vec::with_capacity(fragments);
    for index in (0..fragments).rev() {
        let start = index * LfnEntry::CHARS;
        let mut chars = [0xffffu16; LfnEntry::CHARS];
        let mut wrote_terminator = false;
        for (i, slot) in chars.iter_mut().enumerate() {
            match units.get(start + i) {
                Some(&u) => *slot = u,
                None if !wrote_terminator => {
                    // One NUL ends the name; the rest of the fragment is 0xffff
                    // padding. A fragment that exactly fills has no terminator
                    // at all, which is why this is conditional.
                    *slot = 0;
                    wrote_terminator = true;
                }
                None => {}
            }
        }
        let mut sequence = (index + 1) as u8;
        if index == fragments - 1 {
            sequence |= LfnEntry::LAST;
        }
        out.push(LfnEntry {
            sequence,
            chars,
            checksum,
        });
    }
    out
}

/// Assemble a long name from its fragments, given in the order found on disk.
///
/// Returns `None` when the set is not a well-formed name: a missing fragment, a
/// sequence out of order, or a checksum that does not match the short entry the
/// fragments precede. Any of those means the short entry was changed by
/// something that did not know the long name was there, and the long name is
/// then stale rather than merely inconvenient.
pub fn decode_lfn(fragments: &[LfnEntry], short_checksum: u8) -> Option<String> {
    if fragments.is_empty() {
        return None;
    }
    if fragments.iter().any(|f| f.checksum != short_checksum) {
        return None;
    }
    if fragments[0].sequence & LfnEntry::LAST == 0 {
        return None;
    }

    let count = (fragments[0].sequence & !LfnEntry::LAST) as usize;
    if count != fragments.len() {
        return None;
    }
    for (i, f) in fragments.iter().enumerate() {
        if (f.sequence & !LfnEntry::LAST) as usize != count - i {
            return None;
        }
    }

    let mut units: Vec<u16> = Vec::with_capacity(count * LfnEntry::CHARS);
    for f in fragments.iter().rev() {
        for &c in &f.chars {
            if c == 0 || c == 0xffff {
                break;
            }
            units.push(c);
        }
    }
    String::from_utf16(&units).ok()
}

/// Do two names refer to the same file?
///
/// FAT is case-insensitive and case-preserving, so `Readme.TXT` finds
/// `README.txt`. Folding is ASCII-only, which is what the on-disk short name
/// can express; two long names differing only outside ASCII case are treated as
/// different, as they are on Linux.
pub fn names_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(name: &str) -> Option<String> {
        exact_short_name(name).map(|s| s.to_string_lossy())
    }

    #[test]
    fn names_that_fit_need_no_long_entry() {
        assert_eq!(short("README.TXT").as_deref(), Some("README.TXT"));
        assert_eq!(short("readme.txt").as_deref(), Some("readme.txt"));
        assert_eq!(short("EFI"), Some("EFI".to_string()));
        assert_eq!(short("boot"), Some("boot".to_string()));
        assert_eq!(short("A"), Some("A".to_string()));
        assert_eq!(short("12345678.123").as_deref(), Some("12345678.123"));
    }

    #[test]
    fn the_case_flags_carry_a_lowercase_name() {
        let s = exact_short_name("readme.txt").unwrap();
        assert_eq!(&s.raw, b"README  TXT");
        assert_eq!(s.lcase, LCASE_BASE | LCASE_EXT);

        // One half lowercase, the other not.
        let s = exact_short_name("readme.TXT").unwrap();
        assert_eq!(s.lcase, LCASE_BASE);
    }

    #[test]
    fn names_that_do_not_fit_are_refused_so_a_long_name_is_used() {
        assert_eq!(short("ReadMe.txt"), None, "mixed case within a half");
        assert_eq!(short("thisnameistoolong.txt"), None, "base over eight");
        assert_eq!(short("name.html"), None, "extension over three");
        assert_eq!(short("two.dots.txt"), None, "more than one dot");
        assert_eq!(short(".config"), None, "a leading dot");
        assert_eq!(short("with space.txt"), None, "a space");
        assert_eq!(short("café.txt"), None, "outside ASCII");
    }

    #[test]
    fn a_generated_short_name_gets_a_numeric_tail() {
        let s = generate_short_name("Program Files", |_| false).unwrap();
        assert_eq!(&s.raw, b"PROGRA~1   ");

        let s = generate_short_name("thisnameistoolong.txt", |_| false).unwrap();
        assert_eq!(&s.raw, b"THISNA~1TXT");
    }

    #[test]
    fn a_collision_climbs_the_number() {
        let taken = |raw: &[u8; 11]| raw == b"PROGRA~1   " || raw == b"PROGRA~2   ";
        let s = generate_short_name("Program Files", taken).unwrap();
        assert_eq!(&s.raw, b"PROGRA~3   ");
    }

    #[test]
    fn long_names_round_trip_through_their_fragments() {
        for name in [
            "a",
            "a name that needs exactly two fragments!",
            "thirteenchars",
            "twenty six characters here",
            "A Long File Name With Mixed Case.txt",
            "café ☕.txt",
            &"x".repeat(255),
        ] {
            let fragments = encode_lfn(name, 0x5a);
            assert_eq!(
                decode_lfn(&fragments, 0x5a).as_deref(),
                Some(name),
                "round trip of {name:?}"
            );
            // The first fragment on disk is the last of the name.
            assert!(fragments[0].sequence & LfnEntry::LAST != 0);
            assert_eq!(fragments.last().unwrap().sequence & !LfnEntry::LAST, 1);
        }
    }

    /// A name whose length is an exact multiple of thirteen has no room for the
    /// terminating NUL, and a decoder that insists on one loses the last
    /// character. This is the classic long-name bug.
    #[test]
    fn a_name_that_exactly_fills_its_fragments_keeps_every_character() {
        let name = "thirteenchars";
        assert_eq!(name.len(), 13);
        let fragments = encode_lfn(name, 1);
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].chars.iter().position(|&c| c == 0), None);
        assert_eq!(decode_lfn(&fragments, 1).as_deref(), Some(name));
    }

    #[test]
    fn fragments_whose_checksum_does_not_match_are_rejected() {
        let fragments = encode_lfn("something.txt", 0x11);
        assert_eq!(decode_lfn(&fragments, 0x22), None);
    }

    #[test]
    fn a_missing_fragment_is_rejected() {
        let fragments = encode_lfn(&"y".repeat(40), 7);
        assert!(fragments.len() > 2);
        assert_eq!(decode_lfn(&fragments[1..], 7), None);
    }

    #[test]
    fn names_are_matched_without_regard_to_case() {
        assert!(names_equal("README.txt", "readme.TXT"));
        assert!(!names_equal("readme.txt", "readme.txtx"));
    }

    #[test]
    fn impossible_names_are_refused() {
        assert!(validate("").is_err());
        assert!(validate(".").is_err());
        assert!(validate("..").is_err());
        assert!(validate("a/b").is_err());
        assert!(validate("trailing.").is_err());
        assert!(validate("trailing ").is_err());
        assert!(validate(&"x".repeat(256)).is_err());
        assert!(validate("perfectly fine.txt").is_ok());
    }
}
