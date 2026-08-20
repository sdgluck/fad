//! One `lstat` worth of facts about a directory entry.

use std::fs::Metadata;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// Everything we keep from a stat call. Deliberately `Copy` and small so batches
/// of these move through the channel without allocating per entry.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// Bytes actually allocated on disk (`st_blocks * 512`). This is what `du`
    /// reports and the only honest answer for sparse or compressed files.
    pub blocks: u64,
    /// `st_size`: what the file claims to be. Shown when it diverges from `blocks`.
    pub len: u64,
    pub mtime: i64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub kind: Kind,
}

impl Meta {
    pub fn from_metadata(m: &Metadata) -> Self {
        let ft = m.file_type();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_symlink() {
            Kind::Symlink
        } else if ft.is_file() {
            Kind::File
        } else if ft.is_socket() || ft.is_fifo() || ft.is_block_device() || ft.is_char_device() {
            Kind::Other
        } else {
            Kind::Other
        };
        Meta {
            blocks: m.blocks().saturating_mul(512),
            len: m.size(),
            mtime: m.mtime(),
            dev: m.dev(),
            ino: m.ino(),
            nlink: m.nlink(),
            kind,
        }
    }

    /// Build directly from a raw `stat`, skipping the `std::fs::Metadata` round trip.
    pub fn from_stat(st: &libc::stat) -> Self {
        let fmt = st.st_mode & libc::S_IFMT;
        let kind = match fmt {
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFLNK => Kind::Symlink,
            libc::S_IFREG => Kind::File,
            _ => Kind::Other,
        };
        Meta {
            blocks: (st.st_blocks as u64).saturating_mul(512),
            len: st.st_size as u64,
            mtime: st.st_mtime as i64,
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            nlink: st.st_nlink as u64,
            kind,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}
