/*Es una implementación temporal del backend real. Incluye:
un superblock falso, unos inodos falsos, unos bloques falsos. Se usa sólo para que el fsck:
compile, se ejecute, produzca un reporte*/

use super::{fsck_backend::FsckBackend, fsck_types::*};

pub struct MockBackend {
    pub superblock: Superblock,
    pub inodes: Vec<Inode>,
    pub blocks: Vec<Vec<u8>>,
    pub dirs: Vec<Vec<Dirent>>,
    pub bitmap: Vec<bool>,
}

impl FsckBackend for MockBackend {
    fn load_superblock(&self) -> Superblock {
        self.superblock.clone()
    }

    fn read_inode(&self, ino: u32) -> Option<Inode> {
        self.inodes.get(ino as usize).cloned()
    }

    fn read_block(&self, block: u32) -> Option<Vec<u8>> {
        self.blocks.get(block as usize).cloned()
    }

    fn read_dir(&self, ino: u32) -> Vec<Dirent> {
        self.dirs.get(ino as usize).cloned().unwrap_or_default()
    }

    fn load_all_inodes(&self) -> Vec<Inode> {
        self.inodes.clone()
    }
    fn load_block_bitmap(&self) -> Vec<bool> {
        self.bitmap.clone()
    }
}
