//! Errors surfaced by file I/O.

/// Anything that can go wrong reading or writing files in a FAT filesystem.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem layer failed — a device error, or a structure that did
    /// not decode.
    #[error(transparent)]
    Fs(#[from] mkfs_dos::Error),

    /// No such file or directory.
    #[error("{0}: no such file or directory")]
    NotFound(String),

    /// The path names something that already exists.
    #[error("{0}: already exists")]
    Exists(String),

    /// A directory was expected and something else was found.
    #[error("{0}: not a directory")]
    NotADirectory(String),

    /// A file was expected and a directory was found.
    #[error("{0}: is a directory")]
    IsADirectory(String),

    /// A directory that still has entries in it.
    #[error("{0}: directory not empty")]
    NotEmpty(String),

    /// The volume has no free clusters left.
    #[error("no space left: {needed} more clusters are needed and {free} are free")]
    NoSpace {
        /// Clusters the operation needs.
        needed: u32,
        /// Clusters available.
        free: u32,
    },

    /// The fixed root directory of a FAT12 or FAT16 volume is full.
    ///
    /// It cannot grow — its size was fixed when the filesystem was made. This
    /// is the one limit FAT32 does not have, its root being an ordinary chain.
    #[error("the root directory is full: it holds {entries} entries and cannot grow (FAT12/FAT16)")]
    RootDirectoryFull {
        /// Entries the root directory has room for.
        entries: u32,
    },

    /// A name FAT cannot store.
    #[error("invalid name '{name}': {reason}")]
    InvalidName {
        /// The name that was refused.
        name: String,
        /// Why.
        reason: String,
    },

    /// A path that does not make sense.
    #[error("invalid path '{0}'")]
    InvalidPath(String),

    /// A file too large for the format.
    ///
    /// A FAT file's size is a 32-bit byte count, so 4 GiB minus one byte is the
    /// hard ceiling. It is not a limit of this implementation.
    #[error("{path} would be {size} bytes; FAT stores a size in 32 bits, so 4294967295 is the maximum")]
    FileTooLarge {
        /// The path in question.
        path: String,
        /// The size that was asked for.
        size: u64,
    },

    /// Attribute bits a caller may not set on a file.
    ///
    /// Only read-only, hidden, system and archive are a file's own. The
    /// directory and volume-label bits say what kind of entry it is, and an
    /// ordinary file that claims either is read as something it is not.
    #[error("{path}: attribute bits {bits:#04x} cannot be set; only read-only, hidden, system and archive (0x27) can")]
    InvalidAttributes {
        /// The path in question.
        path: String,
        /// The bits that were asked for.
        bits: u8,
    },

    /// A directory write failed and what reached the device could not be
    /// established.
    ///
    /// The volume refuses every change and every flush from then on: the
    /// cached FAT no longer knows which directory entries name its chains, and
    /// writing it out could join two files' clusters. The image is to be
    /// thrown away, or checked and repaired with `fsck.fat`.
    #[error("the volume is unusable: {0}")]
    Poisoned(String),

    /// Something FAT has no way to express.
    #[error("{0}")]
    Unsupported(String),
}

impl Error {
    pub(crate) fn invalid_name(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::InvalidName {
            name: name.into(),
            reason: reason.into(),
        }
    }
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
