//! Directories: an array of 32-byte slots, and what it takes to change one.
//!
//! A FAT directory has no index, no ordering and no length field. It is slots,
//! from the first to the last, and four things a slot can be: a short entry, a
//! fragment of a long name belonging to the short entry that follows it, a
//! deleted slot, or the end.
//!
//! Two consequences run through everything here:
//!
//! - **A name is a *set* of slots**, not one. Deleting a file means deleting
//!   its long-name fragments too; leaving them behind gives the next file to
//!   land in that slot a name it never had.
//! - **The first byte that is zero ends the directory.** Everything past it has
//!   never been used. So a search can stop there, and writing into that slot
//!   means the slot after it must be left zero.

use mkfs_dos::structs::dirent::{
    Attributes, DirEntry, LfnEntry, ATTR_LFN, DELETED_FLAG, DIR_ENTRY_LEN,
};

use crate::name::{self, ShortName};

/// One name in a directory, with its long-name fragments already assembled.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The name as a person wrote it: the long name when there is one,
    /// otherwise the short name with its case flags applied.
    pub name: String,
    /// The short entry itself.
    pub entry: DirEntry,
    /// Slot index of the first slot of the set — the first long-name fragment,
    /// or the short entry when there is no long name.
    pub first_slot: usize,
    /// Slot index of the short entry.
    pub short_slot: usize,
}

impl Entry {
    /// Slots this name occupies.
    pub fn slots(&self) -> usize {
        self.short_slot - self.first_slot + 1
    }

    /// Is this a directory?
    pub fn is_dir(&self) -> bool {
        self.entry.attr.contains(Attributes::DIRECTORY)
    }

    /// First cluster of the entry's chain, zero for an empty file.
    pub fn first_cluster(&self) -> u32 {
        self.entry.first_cluster()
    }
}

/// Every live name in a directory, in slot order.
///
/// Deleted slots and the volume label are skipped; long-name fragments whose
/// checksum does not match the entry they precede are ignored, and the short
/// name is used instead — which is what a driver does when something without
/// long-name support has renamed the file underneath them.
pub fn parse(data: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut fragments: Vec<LfnEntry> = Vec::new();
    let mut set_start: Option<usize> = None;

    for (slot, chunk) in data.chunks_exact(DIR_ENTRY_LEN).enumerate() {
        if chunk[0] == 0 {
            break;
        }
        if chunk[0] == DELETED_FLAG {
            fragments.clear();
            set_start = None;
            continue;
        }
        if chunk[11] & ATTR_LFN == ATTR_LFN {
            if fragments.is_empty() {
                set_start = Some(slot);
            }
            fragments.push(LfnEntry::decode(chunk));
            continue;
        }

        let entry = DirEntry::decode(chunk);
        if !entry.is_volume_label() {
            let short = entry.short_name();
            let name = name::decode_lfn(&fragments, entry.short_name_checksum())
                .unwrap_or_else(|| short.clone());
            let first_slot = if name == short {
                slot
            } else {
                set_start.unwrap_or(slot)
            };
            out.push(Entry {
                name,
                entry,
                first_slot,
                short_slot: slot,
            });
        }
        fragments.clear();
        set_start = None;
    }
    out
}

/// The volume label entry, if the directory has one. Only the root does.
pub fn find_label(data: &[u8]) -> Option<(usize, DirEntry)> {
    for (slot, chunk) in data.chunks_exact(DIR_ENTRY_LEN).enumerate() {
        if chunk[0] == 0 {
            break;
        }
        if chunk[0] == DELETED_FLAG || chunk[11] & ATTR_LFN == ATTR_LFN {
            continue;
        }
        let entry = DirEntry::decode(chunk);
        if entry.is_volume_label() {
            return Some((slot, entry));
        }
    }
    None
}

/// The slot holding the end marker — the first slot whose first byte is zero —
/// or the slot count when the directory is full to its last slot.
pub fn end_marker(data: &[u8]) -> usize {
    data.chunks_exact(DIR_ENTRY_LEN)
        .position(|chunk| chunk[0] == 0)
        .unwrap_or(data.len() / DIR_ENTRY_LEN)
}

/// Find `needed` consecutive free slots, or say how many the directory would
/// have to grow by.
///
/// A run may end at the end of the directory, since the slots past the
/// terminator are free as well. They are free whatever bytes they hold: a
/// reader stops at the first zero and never looks past it, so a slot there
/// that still carries an old entry is free all the same (f81439860dd8).
pub fn find_free_run(data: &[u8], needed: usize) -> Option<usize> {
    let total = data.len() / DIR_ENTRY_LEN;
    let end = end_marker(data);
    let mut run = 0usize;
    for slot in 0..total {
        let first = data[slot * DIR_ENTRY_LEN];
        if slot >= end || first == DELETED_FLAG {
            run += 1;
            if run == needed {
                return Some(slot + 1 - needed);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Write a name's slots: the long-name fragments, then the short entry.
///
/// When the slots land on the end marker, the slot after them becomes the
/// new one (see [`write_short`]).
pub fn write_set(data: &mut [u8], at: usize, fragments: &[LfnEntry], entry: &DirEntry) {
    let end = end_marker(data);
    for (i, fragment) in fragments.iter().enumerate() {
        let off = (at + i) * DIR_ENTRY_LEN;
        fragment.encode(&mut data[off..off + DIR_ENTRY_LEN]);
    }
    let last = at + fragments.len();
    let off = last * DIR_ENTRY_LEN;
    entry.encode(&mut data[off..off + DIR_ENTRY_LEN]);
    reseal(data, end, last);
}

/// After writing slots up to `last` into a directory whose end marker was at
/// `end`: if the write reached the marker, zero the slot after it, so the
/// directory still ends right after what was written. Whatever lay past the
/// old marker — an old entry, a long-name fragment — was never part of the
/// directory, and must not become part of it now (f81439860dd8).
fn reseal(data: &mut [u8], end: usize, last: usize) {
    let total = data.len() / DIR_ENTRY_LEN;
    let next = last + 1;
    if last >= end && next < total {
        data[next * DIR_ENTRY_LEN..(next + 1) * DIR_ENTRY_LEN].fill(0);
    }
}

/// Mark every slot of a name deleted.
///
/// Deleted rather than zeroed: a zero would end the directory, hiding every
/// name after it. That is the classic way to lose a directory's contents while
/// deleting one file from it.
pub fn erase_set(data: &mut [u8], entry: &Entry) {
    for slot in entry.first_slot..=entry.short_slot {
        data[slot * DIR_ENTRY_LEN] = DELETED_FLAG;
    }
}

/// Write one short entry: in place over an existing one, leaving its name and
/// slots alone, or into a free slot (the volume label). A slot that was the
/// end marker hands the marker on to the slot after it.
pub fn write_short(data: &mut [u8], slot: usize, entry: &DirEntry) {
    let end = end_marker(data);
    let off = slot * DIR_ENTRY_LEN;
    entry.encode(&mut data[off..off + DIR_ENTRY_LEN]);
    reseal(data, end, slot);
}

/// Build the slots for a name: a short name that fits needs no fragments, and
/// one that does not gets a generated short name and a full set.
pub fn build_set(name: &str, taken: &[Entry]) -> crate::error::Result<(Vec<LfnEntry>, ShortName)> {
    name::validate(name)?;

    // A short name is taken when another entry has it as its alias, and also
    // when another entry has it as its long name: a reader that asks for
    // `FOO~1.TXT` must not find two files answering to it.
    let occupied = |raw: &[u8; 11]| {
        let shown = DirEntry {
            name: *raw,
            ..Default::default()
        }
        .short_name();
        taken
            .iter()
            .any(|e| &e.entry.name == raw || name::names_equal(&e.name, &shown))
    };

    // A name that is itself a valid 8.3 name, already answered to by another
    // entry, is that entry's name. Giving it a second alias and storing the
    // name as a long name would make one path name two files: a driver that
    // matches the short name first finds the old one (43bd3bc2b137).
    if let Some(short) = name::exact_short_name(name) {
        if occupied(&short.raw) {
            return Err(crate::error::Error::Exists(name.to_string()));
        }
        return Ok((Vec::new(), short));
    }

    let short = name::generate_short_name(name, occupied)?;
    let checksum = DirEntry {
        name: short.raw,
        ..Default::default()
    }
    .short_name_checksum();
    Ok((name::encode_lfn(name, checksum), short))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dir(slots: usize) -> Vec<u8> {
        vec![0u8; slots * DIR_ENTRY_LEN]
    }

    fn add(data: &mut [u8], name: &str, attr: Attributes, cluster: u32) {
        let existing = parse(data);
        let (fragments, short) = build_set(name, &existing).unwrap();
        let at = find_free_run(data, fragments.len() + 1).unwrap();
        let mut entry = DirEntry {
            name: short.raw,
            lcase: short.lcase,
            attr,
            ..Default::default()
        };
        entry.set_first_cluster(cluster);
        write_set(data, at, &fragments, &entry);
    }

    #[test]
    fn a_short_name_takes_one_slot_and_a_long_name_takes_more() {
        let mut data = empty_dir(32);
        add(&mut data, "README.TXT", Attributes::ARCHIVE, 5);
        add(&mut data, "A Long File Name.txt", Attributes::ARCHIVE, 9);

        let entries = parse(&data);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "README.TXT");
        assert_eq!(entries[0].slots(), 1);
        assert_eq!(entries[1].name, "A Long File Name.txt");
        assert_eq!(entries[1].slots(), 3, "20 characters need two fragments");
        assert_eq!(entries[1].first_cluster(), 9);
    }

    #[test]
    fn a_lowercase_name_that_fits_needs_no_fragments() {
        let mut data = empty_dir(8);
        add(&mut data, "readme.txt", Attributes::ARCHIVE, 2);
        let entries = parse(&data);
        assert_eq!(entries[0].name, "readme.txt");
        assert_eq!(entries[0].slots(), 1);
    }

    #[test]
    fn deleting_a_name_takes_every_slot_of_it_and_leaves_the_rest() {
        let mut data = empty_dir(32);
        add(&mut data, "A Long File Name.txt", Attributes::ARCHIVE, 9);
        add(&mut data, "AFTER.TXT", Attributes::ARCHIVE, 12);

        let entries = parse(&data);
        erase_set(&mut data, &entries[0]);

        let left = parse(&data);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "AFTER.TXT", "the name after it survived");
        // Every slot of the deleted set is marked, not just the short entry.
        for slot in 0..3 {
            assert_eq!(data[slot * DIR_ENTRY_LEN], DELETED_FLAG);
        }
    }

    #[test]
    fn a_deleted_run_is_reused() {
        let mut data = empty_dir(8);
        add(&mut data, "ONE.TXT", Attributes::ARCHIVE, 2);
        add(&mut data, "TWO.TXT", Attributes::ARCHIVE, 3);
        let entries = parse(&data);
        erase_set(&mut data, &entries[0]);

        assert_eq!(find_free_run(&data, 1), Some(0));
        add(&mut data, "THREE.TXT", Attributes::ARCHIVE, 4);
        let names: Vec<_> = parse(&data).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["THREE.TXT", "TWO.TXT"]);
    }

    #[test]
    fn a_full_directory_has_no_run() {
        let mut data = empty_dir(2);
        add(&mut data, "ONE.TXT", Attributes::ARCHIVE, 2);
        add(&mut data, "TWO.TXT", Attributes::ARCHIVE, 3);
        assert_eq!(find_free_run(&data, 1), None);
    }

    #[test]
    fn colliding_long_names_get_different_short_names() {
        let mut data = empty_dir(64);
        add(&mut data, "A Long File Name.txt", Attributes::ARCHIVE, 2);
        add(&mut data, "A Long File Name Too.txt", Attributes::ARCHIVE, 3);

        let entries = parse(&data);
        assert_eq!(entries[0].entry.name, *b"ALONGF~1TXT");
        assert_eq!(entries[1].entry.name, *b"ALONGF~2TXT");
        assert_eq!(entries[1].name, "A Long File Name Too.txt");
    }

    #[test]
    fn the_volume_label_is_not_a_file() {
        let mut data = empty_dir(8);
        let label = DirEntry {
            name: *b"MYVOLUME   ",
            attr: Attributes::VOLUME_ID,
            ..Default::default()
        };
        write_short(&mut data, 0, &label);
        add(&mut data, "FILE.TXT", Attributes::ARCHIVE, 2);

        assert_eq!(parse(&data).len(), 1);
        assert_eq!(find_label(&data).unwrap().0, 0);
    }

    /// bbc3584c0279: a generated alias must not be another entry's long name,
    /// or a reader asking for that name finds two files.
    #[test]
    fn a_generated_alias_avoids_another_entrys_long_name() {
        let mut data = empty_dir(16);
        // A long name that looks like an alias, stored under another short
        // name — as a writer that found FOOBAR~1 taken would have left it.
        let short = *b"OTHER   TXT";
        let checksum = DirEntry {
            name: short,
            ..Default::default()
        }
        .short_name_checksum();
        let fragments = name::encode_lfn("FOOBAR~1.TXT", checksum);
        let entry = DirEntry {
            name: short,
            attr: Attributes::ARCHIVE,
            ..Default::default()
        };
        write_set(&mut data, 0, &fragments, &entry);
        assert_eq!(parse(&data)[0].name, "FOOBAR~1.TXT");

        let (_, generated) = build_set("Foo Bar.txt", &parse(&data)).unwrap();
        assert_eq!(&generated.raw, b"FOOBAR~2TXT");
    }

    /// A long name whose fragments were left by a rename that something without
    /// long-name support performed no longer matches its short entry. The short
    /// name is the truth in that case.
    #[test]
    fn orphaned_fragments_fall_back_to_the_short_name() {
        let mut data = empty_dir(16);
        add(&mut data, "A Long File Name.txt", Attributes::ARCHIVE, 2);
        // Rewrite the short entry's name, as DOS would.
        let mut entry = parse(&data)[0].entry.clone();
        let before = entry.short_name_checksum();
        entry.name = *b"RENAMED TXT";
        // The checksum is one byte, so two names can share one — and this pair
        // does. Pick a name that really differs, or the test proves nothing.
        assert_eq!(entry.short_name_checksum(), before, "this pair collides");
        entry.name = *b"OTHERNAMTXT";
        assert_ne!(entry.short_name_checksum(), before);
        write_short(&mut data, 2, &entry);

        let entries = parse(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "OTHERNAM.TXT");
    }

    /// A directory with an end marker at `end` and, past it, a stale short
    /// entry and a stale long-name fragment, as an old image may hold.
    fn stale_past_the_end(slots: usize, end: usize) -> Vec<u8> {
        let mut data = empty_dir(slots);
        for i in 0..end {
            add(&mut data, &format!("F{i}.TXT"), Attributes::ARCHIVE, 2 + i as u32);
        }
        let stale = DirEntry {
            name: *b"STALE   TXT",
            attr: Attributes::ARCHIVE,
            ..Default::default()
        };
        stale.encode(&mut data[(end + 1) * DIR_ENTRY_LEN..(end + 2) * DIR_ENTRY_LEN]);
        let fragment = name::encode_lfn("Stale long name.txt", 0x5a);
        fragment[0].encode(&mut data[(end + 2) * DIR_ENTRY_LEN..(end + 3) * DIR_ENTRY_LEN]);
        let stale2 = DirEntry {
            name: *b"STALE2  TXT",
            attr: Attributes::ARCHIVE,
            ..Default::default()
        };
        stale2.encode(&mut data[(end + 3) * DIR_ENTRY_LEN..(end + 4) * DIR_ENTRY_LEN]);
        assert_eq!(end_marker(&data), end);
        assert_eq!(parse(&data).len(), end, "the stale bytes are past the end");
        data
    }

    /// f81439860dd8: a one-slot name written on the end marker moves the
    /// marker on, so the stale entry behind it does not come to life.
    #[test]
    fn writing_on_the_end_marker_keeps_stale_slots_unreachable() {
        let mut data = stale_past_the_end(16, 2);
        add(&mut data, "NEW.TXT", Attributes::ARCHIVE, 9);
        let names: Vec<_> = parse(&data).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["F0.TXT", "F1.TXT", "NEW.TXT"]);
        assert_eq!(end_marker(&data), 3);

        // A long name spanning the old marker: same.
        let mut data = stale_past_the_end(16, 2);
        add(&mut data, "A Long File Name.txt", Attributes::ARCHIVE, 9);
        let names: Vec<_> = parse(&data).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["F0.TXT", "F1.TXT", "A Long File Name.txt"]);
        assert_eq!(end_marker(&data), 5);
    }

    /// f81439860dd8: slots past the end marker are free whatever they hold.
    #[test]
    fn slots_past_the_end_marker_are_free_whatever_they_hold() {
        let data = stale_past_the_end(8, 2);
        // Slots 2..8 are free: six of them, though 3, 4 and 5 are not zero.
        assert_eq!(find_free_run(&data, 6), Some(2));
        assert_eq!(find_free_run(&data, 7), None);

        // A deleted slot just before the marker joins the run past it.
        let mut data = stale_past_the_end(8, 2);
        data[DIR_ENTRY_LEN] = DELETED_FLAG;
        assert_eq!(find_free_run(&data, 3), Some(1));
    }

    /// f81439860dd8: the volume label, written on the end marker, moves it on.
    #[test]
    fn a_label_on_the_end_marker_moves_it_on() {
        let mut data = stale_past_the_end(8, 1);
        let at = find_free_run(&data, 1).unwrap();
        assert_eq!(at, 1);
        let label = DirEntry {
            name: *b"MYVOLUME   ",
            attr: Attributes::VOLUME_ID,
            ..Default::default()
        };
        write_short(&mut data, at, &label);
        assert_eq!(find_label(&data).unwrap().0, 1);
        assert_eq!(end_marker(&data), 2);
        let names: Vec<_> = parse(&data).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["F0.TXT"]);
    }

    /// A write that fills the directory's last slot has no slot after it to
    /// clear, and the directory simply ends with its last cluster.
    #[test]
    fn the_last_slot_needs_no_marker_after_it() {
        let mut data = empty_dir(2);
        add(&mut data, "ONE.TXT", Attributes::ARCHIVE, 2);
        add(&mut data, "TWO.TXT", Attributes::ARCHIVE, 3);
        assert_eq!(end_marker(&data), 2);
        assert_eq!(parse(&data).len(), 2);
    }

    /// 43bd3bc2b137: an 8.3 name that is already another entry's alias is
    /// that entry's name, not a new one with a second alias.
    #[test]
    fn an_exact_name_taken_as_an_alias_is_refused() {
        let mut data = empty_dir(16);
        add(&mut data, "Program Files", Attributes::DIRECTORY, 2);
        assert_eq!(parse(&data)[0].entry.name, *b"PROGRA~1   ");
        for name in ["PROGRA~1", "progra~1"] {
            assert!(matches!(
                build_set(name, &parse(&data)),
                Err(crate::error::Error::Exists(_))
            ), "{name}");
        }
        // And an exact name that is another entry's long name.
        add(&mut data, "OTHER.TXT", Attributes::ARCHIVE, 3);
        assert!(matches!(
            build_set("other.txt", &parse(&data)),
            Err(crate::error::Error::Exists(_))
        ));
        // A long name still gets the next free alias.
        let (_, short) = build_set("Program Files 2", &parse(&data)).unwrap();
        assert_eq!(&short.raw, b"PROGRA~2   ");
    }
}
