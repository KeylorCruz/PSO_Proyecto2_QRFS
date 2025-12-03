/*Define todas las estructuras básicas del fsck, incluyendo:
Superblock simplificado
Inode simplificado
Dirent
FsckReport (donde se reportan errores) */

#[derive(Debug, Clone)]
pub struct Superblock {
    pub magic: u32,
    pub num_inodes: u32,
    pub num_blocks: u32,
    pub root_inode: u32,
}

#[derive(Debug, Clone)]
pub struct Inode {
    pub is_dir: bool,
    pub size: u32,
    pub direct: Vec<u32>,
    pub indirect1: Option<u32>,
    pub indirect2: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Dirent {
    pub inode: u32,
    pub name: String,
    pub is_dir: bool,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub struct Bitmap {
    pub blocks: Vec<bool>, // true = usado, false = libre
}

#[derive(Debug)]
pub struct FsckReport {
    pub blocks_ok: bool,
    pub inodes_ok: bool,
    pub errors: Vec<String>,
}

impl FsckReport {
    pub fn new() -> Self {
        Self {
            blocks_ok: true,
            inodes_ok: true,
            errors: Vec::new(),
        }
    }
}
