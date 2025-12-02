// src/lib.rs
mod fs;
mod dir;

pub use crate::fs::QrfsFilesystem;
pub use crate::fs::{SuperblockDisk, InodeDisk, QRFS_BLOCK_SIZE, QRFS_MAGIC, QRFS_VERSION};