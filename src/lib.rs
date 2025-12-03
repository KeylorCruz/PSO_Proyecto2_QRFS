mod fs;
mod dir;
pub mod fsck; // <- descomentar

pub use crate::fs::QrfsFilesystem;
pub use crate::fs::{
    SuperblockDisk,
    InodeDisk,
    DirEntryDisk,
    QRFS_BLOCK_SIZE,
    QRFS_MAGIC,
    QRFS_VERSION,
    QRFS_NAME_LEN,
};
