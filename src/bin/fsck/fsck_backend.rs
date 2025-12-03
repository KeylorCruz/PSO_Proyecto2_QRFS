/*Define la interfaz para el backend del fsck.
Define un trait que describe cómo el fsck debe leer:
bloques, inodos, el directorio raíz
Existe para permitir múltiples backends, por ejemplo:
Un mock (lo que usa ahorita), el FS real de compañeros (cuando esté listo), pruebas de fragmentación
*/

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{SuperblockDisk, InodeDisk, DirEntryDisk, QRFS_BLOCK_SIZE};
use super::fsck_backend::FsckBackend;
use super::fsck_types::{Superblock, Inode, Dirent};

pub struct QrfsBackend {
    pub qr_folder: PathBuf,
}

impl QrfsBackend {
    pub fn new(qr_folder: PathBuf) -> Self {
        Self { qr_folder }
    }

    fn get_qr_entries(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&self.qr_folder)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        entries.sort();
        Ok(entries)
    }

    fn read_block_raw(&self, block_index: u32) -> Option<Vec<u8>> {
        let entries = self.get_qr_entries().ok()?;
        let idx = block_index as usize;
        if idx >= entries.len() {
            return None;
        }
        let mut file = File::open(&entries[idx]).ok()?;
        let mut buf = vec![0u8; QRFS_BLOCK_SIZE as usize];
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn load_superblock_disk(&self) -> Option<SuperblockDisk> {
        let buf = self.read_block_raw(0)?;
        if std::mem::size_of::<SuperblockDisk>() > buf.len() {
            return None;
        }
        let sb: SuperblockDisk = unsafe {
            let ptr = buf.as_ptr() as *const SuperblockDisk;
            ptr.read_unaligned()
        };
        Some(sb)
    }

    fn load_inode_disk(&self, ino: u32, sb: &SuperblockDisk, entries: &[PathBuf]) -> Option<InodeDisk> {
        if ino == 0 || ino > sb.max_inodes {
            return None;
        }

        let inode_size = std::mem::size_of::<InodeDisk>();
        let block_size = QRFS_BLOCK_SIZE as usize;
        let total_bytes = (sb.inode_table_blocks as usize) * block_size;

        let first_block = sb.inode_table_start as usize;
        let last_block_excl = first_block + sb.inode_table_blocks as usize;
        if last_block_excl > entries.len() {
            return None;
        }

        let mut buf = Vec::with_capacity(total_bytes);
        for block_idx in first_block..last_block_excl {
            let mut file = File::open(&entries[block_idx]).ok()?;
            let mut block_buf = vec![0u8; block_size];
            file.read_exact(&mut block_buf).ok()?;
            buf.extend_from_slice(&block_buf);
        }

        let idx_bytes = (ino as usize - 1) * inode_size;
        if idx_bytes + inode_size > buf.len() {
            return None;
        }

        let inode: InodeDisk = unsafe {
            let ptr = buf[idx_bytes..].as_ptr() as *const InodeDisk;
            ptr.read_unaligned()
        };

        Some(inode)
    }

    fn read_root_dir(&self, sb: &SuperblockDisk, entries: &[PathBuf]) -> Vec<Dirent> {
        let mut result = Vec::new();

        // Inodo raíz según superblock
        let root_ino = sb.root_inode;
        if root_ino == 0 {
            return result;
        }

        let inode = match self.load_inode_disk(root_ino, sb, entries) {
            Some(i) => i,
            None => return result,
        };

        if inode.file_type != 2 {
            return result;
        }

        let block = inode.direct_blocks[0];
        if block == 0 {
            return result;
        }

        // Leer bloque de datos del root y convertir DirEntryDisk -> Dirent
        let buf = self.read_block_raw(block).unwrap_or_default();
        let entry_size = std::mem::size_of::<DirEntryDisk>();
        let mut offset = 0;

        while offset + entry_size <= buf.len() {
            let disk_entry: DirEntryDisk = unsafe {
                let ptr = buf[offset..].as_ptr() as *const DirEntryDisk;
                ptr.read_unaligned()
            };
            offset += entry_size;

            if disk_entry.inode == 0 {
                continue;
            }

            let name_bytes: Vec<u8> = disk_entry
                .name
                .iter()
                .copied()
                .take_while(|&b| b != 0)
                .collect();
            let name = String::from_utf8_lossy(&name_bytes).to_string();
            if name.is_empty() {
                continue;
            }

            result.push(Dirent {
                inode: disk_entry.inode,
                name,
                is_dir: true, // en QRFS, '.' y '..' en root son directorios
            });
        }

        result
    }
}

impl FsckBackend for QrfsBackend {
    fn load_superblock(&self) -> Superblock {
        // Adaptamos SuperblockDisk al Superblock simplificado de fsck
        if let Some(sb) = self.load_superblock_disk() {
            Superblock {
                magic: 0x1234, // lo que espera fsck.rs
                num_inodes: sb.max_inodes + 1,
                num_blocks: sb.total_blocks,
                root_inode: sb.root_inode, // mismo índice que usamos en Dirent.inode
            }
        } else {
            Superblock {
                magic: 0,
                num_inodes: 0,
                num_blocks: 0,
                root_inode: 0,
            }
        }
    }

    fn load_all_inodes(&self) -> Vec<Inode> {
        let sb_disk = match self.load_superblock_disk() {
            Some(sb) => sb,
            None => return Vec::new(),
        };

        let entries = match self.get_qr_entries() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        let mut result = Vec::new();

        // Índice 0 lo dejamos como "dummy" para que root=1 funcione bien
        result.push(Inode {
            is_dir: false,
            size: 0,
            direct: Vec::new(),
            indirect1: None,
            indirect2: None,
        });

        for ino in 1..=sb_disk.max_inodes {
            if let Some(disk_inode) = self.load_inode_disk(ino, &sb_disk, &entries) {
                let is_dir = disk_inode.file_type == 2;
                let size = disk_inode.size as u32;
                let direct = disk_inode.direct_blocks.to_vec();
                let indirect1 = if disk_inode.indirect_block != 0 {
                    Some(disk_inode.indirect_block)
                } else {
                    None
                };
                let indirect2 = if disk_inode.double_indirect_block != 0 {
                    Some(disk_inode.double_indirect_block)
                } else {
                    None
                };

                result.push(Inode {
                    is_dir,
                    size,
                    direct,
                    indirect1,
                    indirect2,
                });
            } else {
                // Inodo no inicializado -> lo tratamos vacío
                result.push(Inode {
                    is_dir: false,
                    size: 0,
                    direct: Vec::new(),
                    indirect1: None,
                    indirect2: None,
                });
            }
        }

        result
    }

    fn read_inode(&self, ino: u32) -> Option<Inode> {
        let all = self.load_all_inodes();
        all.get(ino as usize).cloned()
    }

    fn read_block(&self, block: u32) -> Option<Vec<u8>> {
        self.read_block_raw(block)
    }

    fn read_dir(&self, ino: u32) -> Vec<Dirent> {
        // Por ahora sólo soportamos el directorio raíz de forma real;
        // el resto se puede extender si fuera necesario.
        let sb_disk = match self.load_superblock_disk() {
            Some(sb) => sb,
            None => return Vec::new(),
        };
        let entries = match self.get_qr_entries() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        if ino == sb_disk.root_inode {
            self.read_root_dir(&sb_disk, &entries)
        } else {
            Vec::new()
        }
    }

    fn load_block_bitmap(&self) -> Vec<bool> {
        let sb_disk = match self.load_superblock_disk() {
            Some(sb) => sb,
            None => return Vec::new(),
        };

        let entries = match self.get_qr_entries() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        let block_size = QRFS_BLOCK_SIZE as usize;
        let total_blocks = sb_disk.total_blocks as usize;
        let first_block = sb_disk.free_bitmap_start as usize;
        let last_block_excl = first_block + sb_disk.free_bitmap_blocks as usize;
        if last_block_excl > entries.len() {
            return Vec::new();
        }

        let mut buf = Vec::new();
        for block_idx in first_block..last_block_excl {
            let mut file = File::open(&entries[block_idx]).ok()?;
            let mut block_buf = vec![0u8; block_size];
            file.read_exact(&mut block_buf).ok()?;
            buf.extend_from_slice(&block_buf);
        }

        let needed_bytes = (total_blocks + 7) / 8;
        buf.truncate(needed_bytes);

        // Pasar a Vec<bool>
        let mut bitmap = vec![false; total_blocks];
        for b in 0..total_blocks {
            let byte = b / 8;
            let bit = (b % 8) as u8;
            if byte < buf.len() && (buf[byte] & (1 << bit)) != 0 {
                bitmap[b] = true;
            }
        }
        bitmap
    }
}
