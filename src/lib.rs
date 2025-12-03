// src/lib.rs
mod fs;
mod dir;

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
