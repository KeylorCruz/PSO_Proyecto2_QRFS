use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::mem;
use std::collections::HashMap;

use crate::dir; // para usar dir::unpack_dir_entries y dir::DirEntry


use anyhow::{Result, Context};
use fuser::{
    FileAttr,
    FileType,
    Filesystem,
    MountOption,
    ReplyAttr,
    ReplyDirectory,
    ReplyEmpty,
    ReplyEntry,
    ReplyCreate,
    ReplyData,
    ReplyWrite,
    ReplyOpen,
    ReplyStatfs,
    Request,
};

use libc::ENOENT;

use crate::dir;

pub const ROOT_INO: u64 = 1;

// -----------------------------------------------------------------------------
// Constantes y estructuras de disco de QRFS
// -----------------------------------------------------------------------------

pub const QRFS_BLOCK_SIZE: u32 = 1024;
pub const QRFS_MAGIC: u32   = 0x5152_4653; 
pub const QRFS_VERSION: u32 = 1;
pub const QRFS_NAME_LEN: usize = 56;

// -------------------- Estructuras en disco --------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SuperblockDisk {
    pub magic: u32,
    pub version: u32,

    pub block_size: u32,
    pub total_blocks: u32,

    pub inode_table_start: u32,
    pub inode_table_blocks: u32,
    pub free_bitmap_start: u32,
    pub free_bitmap_blocks: u32,
    pub data_blocks_start: u32,

    pub max_inodes: u32,
    pub root_inode: u32,

    pub free_blocks: u32,
    pub free_inodes: u32,

    pub reserved: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct InodeDisk {
    pub id: u32,
    pub file_type: u16,
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub nlink: u32,
    pub direct_blocks: [u32; 12],
    pub indirect_block: u32,
    pub double_indirect_block: u32,
    pub _padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntryDisk {
    pub inode: u32,
    pub name: [u8; QRFS_NAME_LEN],
}

// -----------------------------------------------------------------------------
// Estructuras en memoria
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Inode {
    pub ino: u64,
    pub kind: FileType,
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub nlink: u32,
}

impl Inode {
    /// Crea un inodo de tipo directorio con permisos estándar.
    pub fn dir(ino: u64) -> Self {
        let now = SystemTime::now();
        Self {
            ino,
            kind: FileType::Directory,
            perm: 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            atime: now,
            mtime: now,
            ctime: now,
            nlink: 2, // "." y ".."
        }
    }

    /// Crea un inodo de tipo archivo regular.
    pub fn file(ino: u64, size: u64) -> Self {
        let now = SystemTime::now();
        Self {
            ino,
            kind: FileType::RegularFile,
            perm: 0o644,
            uid: 0,
            gid: 0,
            size,
            atime: now,
            mtime: now,
            ctime: now,
            nlink: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Directory {
    pub parent: u64,
    pub entries: HashMap<String, u64>, // nombre -> ino
}

// -------------------- Estado en memoria del FS --------------------

pub struct QrfsInner {
    pub qr_folder: PathBuf,
    pub superblock: SuperblockDisk,
    pub free_blocks: u32,
    pub free_inodes: u32,

    pub inodes: HashMap<u64, Inode>,
    pub directories: HashMap<u64, Directory>,
    pub next_ino: u64,

    // Contenido de archivos regulares en memoria (ino -> bytes)
    pub files: HashMap<u64, Vec<u8>>,
}

#[derive(Clone)]
pub struct QrfsFilesystem {
    inner: Arc<RwLock<QrfsInner>>,
}

// -----------------------------------------------------------------------------
// Conversión de Inode a FileAttr de FUSE
// -----------------------------------------------------------------------------

pub fn inode_to_attr(inode: &Inode) -> FileAttr {
    FileAttr {
        ino: inode.ino,
        size: inode.size,
        blocks: 0,
        atime: inode.atime,
        mtime: inode.mtime,
        ctime: inode.ctime,
        crtime: inode.ctime,
        kind: inode.kind,
        perm: inode.perm,
        nlink: inode.nlink,
        uid: inode.uid,
        gid: inode.gid,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

// -----------------------------------------------------------------------------
// Montaje desde carpeta de QRs + run()
// -----------------------------------------------------------------------------

impl QrfsFilesystem {
    /// Construye el estado interno del FS a partir de una carpeta con QRs.
    /// - Lee el primer archivo como bloque 0 (superblock)
    /// - Valida magic y versión
    /// - Deja en memoria el superblock y contadores de bloques/inodos libres
    /// - Inicializa un root lógico (ino = 1) vacío
    pub fn mount_from_folder(
        qr_folder: &Path,
        _passphrase: Option<String>,
        start_qr: Option<PathBuf>,
    ) -> Result<Self> {
        // 1. Listar archivos de la carpeta de QRs
                let mut entries: Vec<PathBuf> = fs::read_dir(qr_folder)
            .with_context(|| format!("No se pudo leer el directorio {:?}", qr_folder))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .collect();

        entries.sort();

        if entries.is_empty() {
            return Err(anyhow::anyhow!(
                "No se encontraron archivos de bloques en {:?}",
                qr_folder
            ));
        }

        // Si el usuario especificó un archivo de inicio, lo usamos como bloque 0
        if let Some(start) = start_qr {
            // Comparamos por nombre de archivo (no por ruta absoluta)
            if let Some(pos) = entries
                .iter()
                .position(|p| p.file_name() == start.file_name())
            {
                // Ponemos ese archivo en la posición 0 (bloque lógico 0)
                entries.swap(0, pos);
            } else {
                eprintln!(
                    "Advertencia: start_qr {:?} no se encontró en {:?}, se usa el primer archivo",
                    start.file_name(),
                    qr_folder
                );
            }
        }


        // 2. Leer el primer archivo como bloque 0 (superblock)
        let first_block = &entries[0];
        let mut file = File::open(first_block).with_context(|| {
            format!("No se pudo abrir el primer bloque {:?}", first_block)
        })?;

        let mut buf = vec![0u8; QRFS_BLOCK_SIZE as usize];
        file.read_exact(&mut buf)
            .with_context(|| "No se pudo leer el superblock completo")?;

        if mem::size_of::<SuperblockDisk>() > buf.len() {
            return Err(anyhow::anyhow!(
                "SuperblockDisk ({}) es más grande que el bloque ({})",
                mem::size_of::<SuperblockDisk>(),
                buf.len()
            ));
        }

        // Interpretar los bytes como un SuperblockDisk
        let superblock: SuperblockDisk = unsafe {
            let ptr = buf.as_ptr() as *const SuperblockDisk;
            ptr.read_unaligned()
        };

        // 3. Validar que esto parece un QRFS
        if superblock.magic != QRFS_MAGIC {
            return Err(anyhow::anyhow!(
                "El magic del superblock no coincide (esperado = {:#X}, leído = {:#X})",
                QRFS_MAGIC,
                superblock.magic
            ));
        }

        if superblock.version != QRFS_VERSION {
            return Err(anyhow::anyhow!(
                "Versión de FS no soportada (esperado = {}, leído = {})",
                QRFS_VERSION,
                superblock.version
            ));
        }

        // 5. Construir el estado interno leyendo inodos y directorio raíz desde disco
        let mut inodes: HashMap<u64, Inode> = HashMap::new();
        let mut directories: HashMap<u64, Directory> = HashMap::new();
        let mut max_ino_used: u64 = 0;

        let root_ino = superblock.root_inode as u64;

        // 5.1. Cargar todos los inodos válidos desde la tabla de inodos
        for ino in 1..=superblock.max_inodes as u64 {
            let disk_inode = match load_inode_disk(&qr_folder, &superblock, ino) {
                Ok(inode) => inode,
                Err(e) => {
                    eprintln!("Advertencia: no se pudo cargar inodo {} desde disco: {e:?}", ino);
                    continue;
                }
            };

            // Inodo no usado: id = 0 o nlink = 0
            if disk_inode.id == 0 || disk_inode.nlink == 0 {
                continue;
            }

            let kind = match disk_inode.file_type {
                2 => FileType::Directory,
                _ => FileType::RegularFile,
            };

            let atime = UNIX_EPOCH + Duration::from_secs(disk_inode.atime);
            let mtime = UNIX_EPOCH + Duration::from_secs(disk_inode.mtime);
            let ctime = UNIX_EPOCH + Duration::from_secs(disk_inode.ctime);

            let inode = Inode {
                ino,
                kind,
                perm: disk_inode.perm,
                uid: disk_inode.uid,
                gid: disk_inode.gid,
                size: disk_inode.size,
                atime,
                mtime,
                ctime,
                nlink: disk_inode.nlink,
            };

            if ino > max_ino_used {
                max_ino_used = ino;
            }

            inodes.insert(ino, inode);
        }

        // 5.2. Cargar el directorio raíz desde disco
        let mut root_parent = root_ino;
        let mut root_entries_map: HashMap<String, u64> = HashMap::new();

        match read_directory_from_disk(&qr_folder, &superblock, root_ino) {
            Ok(entries) => {
                for e in entries {
                    if e.name == "." {
                        continue;
                    }
                    if e.name == ".." {
                        // Guardamos el parent real del root
                        root_parent = e.ino;
                        continue;
                    }
                    root_entries_map.insert(e.name.clone(), e.ino);
                }
            }
            Err(e) => {
                eprintln!(
                    "Advertencia: no se pudo leer el directorio raíz desde disco: {e:?}. Se inicializa vacío."
                );
            }
        }

        // 5.3. Registrar el directorio raíz en la tabla de directorios
        directories.insert(
            root_ino,
            Directory {
                parent: root_parent,
                entries: root_entries_map,
            },
        );

        // Si por alguna razón no hay ningún inodo usado, garantizamos al menos el root
        if max_ino_used == 0 {
            max_ino_used = root_ino.max(1);
            if !inodes.contains_key(&root_ino) {
                let root_inode = Inode::dir(root_ino);
                inodes.insert(root_ino, root_inode);
            }
        }

        let inner = QrfsInner {
            qr_folder: qr_folder.to_path_buf(),
            superblock,
            free_blocks: superblock.free_blocks,
            free_inodes: superblock.free_inodes,
            inodes,
            directories,
            next_ino: max_ino_used + 1,
            files: HashMap::new(),
        };


        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })

    }

    /// Monta el FS con FUSE en el punto de montaje indicado.
    pub fn run(self, mountpoint: PathBuf) -> Result<()> {
        let options = vec![
            MountOption::FSName("qrfs".to_string()),
            MountOption::AutoUnmount,
            MountOption::RW, // read-write
        ];

        fuser::mount2(self, &mountpoint, &options)?;
        Ok(())
    }
}

fn get_qr_entries(qr_folder: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(qr_folder)
        .with_context(|| format!("No se pudo leer el directorio {:?}", qr_folder))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();

    entries.sort();
    Ok(entries)
}

fn read_fs_block(qr_folder: &Path, block_index: u32) -> Result<Vec<u8>> {
    let entries = get_qr_entries(qr_folder)?;
    let idx = block_index as usize;

    if idx >= entries.len() {
        return Err(anyhow::anyhow!(
            "Índice de bloque fuera de rango: {} (hay {} archivos QR)",
            idx,
            entries.len()
        ));
    }

    let mut file = File::open(&entries[idx])
        .with_context(|| format!("No se pudo abrir el bloque {:?}", entries[idx]))?;

    let mut buf = vec![0u8; QRFS_BLOCK_SIZE as usize];
    file.read_exact(&mut buf)
        .with_context(|| format!("No se pudo leer el bloque completo de {:?}", entries[idx]))?;

    Ok(buf)
}

fn load_inode_disk(qr_folder: &Path, superblock: &SuperblockDisk, ino: u64) -> Result<InodeDisk> {
    if ino == 0 || ino > superblock.max_inodes as u64 {
        return Err(anyhow::anyhow!(
            "Inodo fuera de rango: {} (max_inodes = {})",
            ino,
            superblock.max_inodes
        ));
    }

    let inode_size = mem::size_of::<InodeDisk>();
    let block_size = QRFS_BLOCK_SIZE as usize;
    let total_bytes = (superblock.inode_table_blocks as usize) * block_size;

    let entries = get_qr_entries(qr_folder)?;
    let first_block = superblock.inode_table_start as usize;
    let last_block_excl = first_block + superblock.inode_table_blocks as usize;

    if last_block_excl > entries.len() {
        return Err(anyhow::anyhow!(
            "La tabla de inodos referencia bloques fuera de rango ({}..{} en {} archivos)",
            first_block,
            last_block_excl,
            entries.len()
        ));
    }

    let mut buf = Vec::with_capacity(total_bytes);
    for block_idx in first_block..last_block_excl {
        let mut file = File::open(&entries[block_idx])
            .with_context(|| format!("No se pudo abrir el bloque de inodos {:?}", entries[block_idx]))?;
        let mut block_buf = vec![0u8; block_size];
        file.read_exact(&mut block_buf)
            .with_context(|| format!("No se pudo leer completamente el bloque {:?}", entries[block_idx]))?;
        buf.extend_from_slice(&block_buf);
    }

    let idx_bytes = (ino as usize - 1) * inode_size;
    if idx_bytes + inode_size > buf.len() {
        return Err(anyhow::anyhow!(
            "Inodo {} fuera del rango de la tabla (idx_bytes = {}, len = {})",
            ino,
            idx_bytes,
            buf.len()
        ));
    }

    let inode: InodeDisk = unsafe {
        let ptr = buf[idx_bytes..].as_ptr() as *const InodeDisk;
        ptr.read_unaligned()
    };

    Ok(inode)
}

fn load_bitmap(qr_folder: &Path, superblock: &SuperblockDisk) -> Result<Vec<u8>> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let total_bytes = (superblock.free_bitmap_blocks as usize) * block_size;

    let entries = get_qr_entries(qr_folder)?;
    let first_block = superblock.free_bitmap_start as usize;
    let last_block_excl = first_block + superblock.free_bitmap_blocks as usize;

    if last_block_excl > entries.len() {
        return Err(anyhow::anyhow!(
            "El bitmap referencia bloques fuera de rango ({}..{} en {} archivos)",
            first_block,
            last_block_excl,
            entries.len()
        ));
    }

    let mut buf = Vec::with_capacity(total_bytes);
    for block_idx in first_block..last_block_excl {
        let mut file = File::open(&entries[block_idx])
            .with_context(|| format!("No se pudo abrir el bloque de bitmap {:?}", entries[block_idx]))?;
        let mut block_buf = vec![0u8; block_size];
        file.read_exact(&mut block_buf)
            .with_context(|| format!("No se pudo leer completamente el bloque {:?}", entries[block_idx]))?;
        buf.extend_from_slice(&block_buf);
    }

    // Solo nos interesan los bits hasta total_blocks
    let needed_bytes = ((superblock.total_blocks as usize) + 7) / 8;
    buf.truncate(needed_bytes);
    Ok(buf)
}

fn write_bitmap(qr_folder: &Path, superblock: &SuperblockDisk, bitmap: &[u8]) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let total_bytes = (superblock.free_bitmap_blocks as usize) * block_size;

    let entries = get_qr_entries(qr_folder)?;
    let first_block = superblock.free_bitmap_start as usize;
    let last_block_excl = first_block + superblock.free_bitmap_blocks as usize;

    if last_block_excl > entries.len() {
        return Err(anyhow::anyhow!(
            "El bitmap referencia bloques fuera de rango ({}..{} en {} archivos)",
            first_block,
            last_block_excl,
            entries.len()
        ));
    }

    // Buffer completo de bloques para escribir
    let mut buf = vec![0u8; total_bytes];
    let copy_len = std::cmp::min(bitmap.len(), total_bytes);
    buf[..copy_len].copy_from_slice(&bitmap[..copy_len]);

    for (i, chunk) in buf.chunks(block_size).enumerate() {
        let block_idx = first_block + i;
        let mut file = File::create(&entries[block_idx])
            .with_context(|| format!("No se pudo abrir el bloque de bitmap {:?} para escritura", entries[block_idx]))?;
        file.write_all(chunk)
            .with_context(|| format!("No se pudo escribir completamente el bloque {:?}", entries[block_idx]))?;
    }

    Ok(())
}

fn bitmap_test(bitmap: &[u8], block_index: u32) -> bool {
    let idx = block_index as usize;
    let byte = idx / 8;
    let bit = (idx % 8) as u8;

    if byte >= bitmap.len() {
        return true; // fuera de rango = lo consideramos "ocupado"
    }

    (bitmap[byte] & (1 << bit)) != 0
}

fn bitmap_set(bitmap: &mut [u8], block_index: u32, used: bool) {
    let idx = block_index as usize;
    let byte = idx / 8;
    let bit = (idx % 8) as u8;

    if byte >= bitmap.len() {
        return;
    }

    if used {
        bitmap[byte] |= 1 << bit;
    } else {
        bitmap[byte] &= !(1 << bit);
    }
}

fn write_superblock(qr_folder: &Path, sb: &SuperblockDisk) -> Result<()> {
    let entries = get_qr_entries(qr_folder)?;
    if entries.is_empty() {
        return Err(anyhow::anyhow!(
            "No hay archivos de bloque para escribir el superblock"
        ));
    }

    let block_size = QRFS_BLOCK_SIZE as usize;
    let mut buf = vec![0u8; block_size];
    let sb_size = mem::size_of::<SuperblockDisk>();

    if sb_size > buf.len() {
        return Err(anyhow::anyhow!(
            "SuperblockDisk ({}) es más grande que el bloque ({})",
            sb_size,
            buf.len()
        ));
    }

    unsafe {
        let src = (sb as *const SuperblockDisk) as *const u8;
        let slice = std::slice::from_raw_parts(src, sb_size);
        buf[..sb_size].copy_from_slice(slice);
    }

    let mut file = File::create(&entries[0])
        .with_context(|| format!("No se pudo abrir el bloque 0 ({:?}) para escritura", entries[0]))?;
    file.write_all(&buf)
        .with_context(|| "No se pudo escribir el superblock completo")?;

    Ok(())
}

fn write_inode_disk(
    qr_folder: &Path,
    superblock: &SuperblockDisk,
    ino: u64,
    inode: &InodeDisk,
) -> Result<()> {
    if ino == 0 || ino > superblock.max_inodes as u64 {
        return Err(anyhow::anyhow!(
            "Inodo fuera de rango en write_inode_disk: {} (max_inodes = {})",
            ino,
            superblock.max_inodes
        ));
    }

    let inode_size = mem::size_of::<InodeDisk>();
    let block_size = QRFS_BLOCK_SIZE as usize;
    let total_bytes = (superblock.inode_table_blocks as usize) * block_size;

    let entries = get_qr_entries(qr_folder)?;
    let first_block = superblock.inode_table_start as usize;
    let last_block_excl = first_block + superblock.inode_table_blocks as usize;

    if last_block_excl > entries.len() {
        return Err(anyhow::anyhow!(
            "La tabla de inodos referencia bloques fuera de rango ({}..{} en {} archivos)",
            first_block,
            last_block_excl,
            entries.len()
        ));
    }

    // Leer tabla de inodos completa
    let mut buf = Vec::with_capacity(total_bytes);
    for block_idx in first_block..last_block_excl {
        let mut file = File::open(&entries[block_idx])
            .with_context(|| format!("No se pudo abrir el bloque de inodos {:?}", entries[block_idx]))?;
        let mut block_buf = vec![0u8; block_size];
        file.read_exact(&mut block_buf)
            .with_context(|| format!("No se pudo leer completamente el bloque {:?}", entries[block_idx]))?;
        buf.extend_from_slice(&block_buf);
    }

    let idx_bytes = (ino as usize - 1) * inode_size;
    if idx_bytes + inode_size > buf.len() {
        return Err(anyhow::anyhow!(
            "Inodo {} fuera del rango de la tabla al escribir (idx_bytes = {}, len = {})",
            ino,
            idx_bytes,
            buf.len()
        ));
    }

    unsafe {
        let ptr = inode as *const InodeDisk as *const u8;
        let slice = std::slice::from_raw_parts(ptr, inode_size);
        buf[idx_bytes..idx_bytes + inode_size].copy_from_slice(slice);
    }

    // Escribir tabla de inodos de vuelta
    for (i, chunk) in buf.chunks(block_size).enumerate() {
        let block_idx = first_block + i;
        let mut file = File::create(&entries[block_idx])
            .with_context(|| format!("No se pudo abrir el bloque de inodos {:?} para escritura", entries[block_idx]))?;
        file.write_all(chunk)
            .with_context(|| format!("No se pudo escribir completamente el bloque {:?}", entries[block_idx]))?;
    }

    Ok(())
}

fn write_fs_block(qr_folder: &Path, block_index: u32, data: &[u8]) -> Result<()> {
    let entries = get_qr_entries(qr_folder)?;
    let idx = block_index as usize;

    if idx >= entries.len() {
        return Err(anyhow::anyhow!(
            "Índice de bloque fuera de rango en write_fs_block: {} (hay {} archivos QR)",
            idx,
            entries.len()
        ));
    }

    let block_size = QRFS_BLOCK_SIZE as usize;
    let mut buf = vec![0u8; block_size];
    let len = std::cmp::min(block_size, data.len());
    buf[..len].copy_from_slice(&data[..len]);

    let mut file = File::create(&entries[idx])
        .with_context(|| format!("No se pudo abrir el bloque {:?} para escritura", entries[idx]))?;
    file.write_all(&buf)
        .with_context(|| format!("No se pudo escribir completamente el bloque {:?}", entries[idx]))?;

    Ok(())
}

/// Asigna un bloque de datos libre en el bitmap (versión mínima: busca desde data_blocks_start)
fn alloc_block(inner: &mut QrfsInner) -> Result<u32> {
    let qr_folder = inner.qr_folder.clone();
    let sb = &mut inner.superblock;

    let mut bitmap = load_bitmap(&qr_folder, sb)?;

    for b in sb.data_blocks_start..sb.total_blocks {
        if !bitmap_test(&bitmap, b) {
            // Encontramos un bloque libre
            bitmap_set(&mut bitmap, b, true);

            if inner.free_blocks > 0 {
                inner.free_blocks -= 1;
            }
            if sb.free_blocks > 0 {
                sb.free_blocks -= 1;
            }

            write_bitmap(&qr_folder, sb, &bitmap)?;
            write_superblock(&qr_folder, sb)?;
            return Ok(b);
        }
    }

    Err(anyhow::anyhow!("No hay bloques de datos libres disponibles"))
}

fn read_directory_from_disk(
    qr_folder: &Path,
    superblock: &SuperblockDisk,
    ino: u64,
) -> Result<Vec<dir::DirEntry>> {
    // Cargar el inodo del directorio
    let inode_disk = load_inode_disk(qr_folder, superblock, ino)?;

    if inode_disk.file_type != 2 {
        return Err(anyhow::anyhow!(
            "Inodo {} no es un directorio (file_type = {})",
            ino,
            inode_disk.file_type
        ));
    }

    // Versión mínima: suponemos que el directorio cabe en el primer bloque directo
    let data_block = inode_disk.direct_blocks[0];
    if data_block == 0 {
        // Directorio vacío
        return Ok(Vec::new());
    }

    // Leer el bloque de datos correspondiente al directorio
    let buf = read_fs_block(qr_folder, data_block)?;

    // Usar el helper del módulo dir para desempaquetar las entradas DirEntryDisk
    let entries = dir::unpack_dir_entries(&buf);

    Ok(entries)
}



// -----------------------------------------------------------------------------
// Implementación FUSE 
// -----------------------------------------------------------------------------

impl Filesystem for QrfsFilesystem {
    
    // getattr: info de un inodo
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        println!("getattr llamado: ino = {ino}");
        let inner = self.inner.read().unwrap();

        if let Some(inode) = inner.inodes.get(&ino) {
            let attr = inode_to_attr(inode);
            let ttl = Duration::from_secs(1);
            reply.attr(&ttl, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    // lookup: resolver (parent, nombre) -> inodo
    fn lookup(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: ReplyEntry,
    ) {
        println!("lookup llamado: parent = {parent}, name = {:?}", name);
        let inner = self.inner.read().unwrap();

        let name_str = name.to_string_lossy().to_string();

        // Buscar el directorio padre
        let dir = match inner.directories.get(&parent) {
            Some(d) => d,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_ino = match dir.entries.get(&name_str) {
            Some(ino) => ino,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let inode = match inner.inodes.get(child_ino) {
            Some(i) => i,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let attr = inode_to_attr(inode);
        let ttl = Duration::from_secs(1);
        reply.entry(&ttl, &attr, 0);
    }

    // access: por ahora sólo dejamos pasar el root, resto ENOENT
    fn access(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mask: i32,
        reply: ReplyEmpty,
    ) {
        println!("access llamado: ino = {ino}");

        if ino == ROOT_INO {
            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }

    // opendir
    fn opendir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _flags: i32,
        reply: ReplyOpen,
    ) {
        println!("opendir llamado");
        let inner = self.inner.read().unwrap();
        if !dir::is_directory(&inner, ino) {
            reply.error(libc::ENOTDIR);
            return;
        }

        // Versión mínima: aceptamos siempre y usamos el propio ino como "file handle"
        let fh = ino;
        reply.opened(fh, 0);
    }

    // readdir
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        println!("readdir llamado: ino = {ino}, offset = {offset}");
        let inner = self.inner.read().unwrap();

        let entries = match dir::list_directory(&inner, ino) {
            Ok(e) => e,
            Err(_) => {
                reply.error(ENOENT);
                return;
            }
        };

        let mut offset_i = offset as usize;

        // "." (offset 0)
        if offset_i == 0 {
            let full = reply.add(ino, 1, FileType::Directory, ".");
            if full {
                reply.ok();
                return;
            }
            offset_i = 1;
        }

        // ".." (offset 1)
        if offset_i == 1 {
            let parent = dir::parent_inode(&inner, ino).unwrap_or(ino);
            let full = reply.add(parent, 2, FileType::Directory, "..");
            if full {
                reply.ok();
                return;
            }
            offset_i = 2;
        }

        // Resto de entradas (offset >= 2)
        for (i, e) in entries.iter().enumerate().skip(offset_i - 2) {
            let next_offset = (i + 3) as i64;
            let full = reply.add(e.ino, next_offset, e.file_type, &e.name);
            if full {
                break;
            }
        }

        reply.ok();
    }

    // mkdir (delegado a dir.rs)
    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        println!("mkdir llamado: parent = {parent}, name = {:?}", name);
        let mut inner = self.inner.write().unwrap();
        match dir::create_directory(&mut inner, parent, name, mode) {
            Ok(attr) => reply.entry(&Duration::from_secs(1), &attr, 0),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // rmdir (delegado a dir.rs)
    fn rmdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: ReplyEmpty,
    ) {
        println!("rmdir llamado: parent = {parent}, name = {:?}", name);
        let mut inner = self.inner.write().unwrap();
        match dir::remove_directory(&mut inner, parent, name) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // rename (delegado a dir.rs)
    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        println!(
            "rename llamado: parent = {parent}, name = {:?}, newparent = {newparent}, newname = {:?}",
            name, newname
        );
        let mut inner = self.inner.write().unwrap();
        match dir::rename_entry(&mut inner, parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // statfs: estadísticas del FS (usa el superblock)
    fn statfs(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        reply: ReplyStatfs,
    ) {
        let inner = self.inner.read().unwrap();
        let sb = &inner.superblock;

        let blocks  = sb.total_blocks as u64;
        let bfree   = inner.free_blocks as u64;
        let bavail  = bfree;
        let files   = sb.max_inodes as u64;
        let ffree   = inner.free_inodes as u64;
        let bsize   = sb.block_size as u32;
        let namelen = 255;
        let frsize  = sb.block_size as u32;

        reply.statfs(
            blocks,
            bfree,
            bavail,
            files,
            ffree,
            bsize,
            namelen,
            frsize,
        );
    }

    // fsync: por ahora, sólo trazamos y respondemos ok
    fn fsync(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        println!("fsync llamado: ino = {ino}");
        // Más adelante: forzar flush real hacia los QRs físicos.
        reply.ok();
    }

    // open
    fn open(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        flags: i32,
        reply: ReplyOpen,
    ) {
        println!("open llamado: ino = {ino}, flags = {flags}");

        // Versión mínima: comprobamos que el inodo exista.
        let inner = self.inner.read().unwrap();
        if !inner.inodes.contains_key(&ino) {
            reply.error(ENOENT);
            return;
        }

        // Versión mínima: aceptamos siempre y usamos el propio ino como "file handle"
        let fh = ino;
        reply.opened(fh, 0);
    }

    // create
    fn create(
    &mut self,
    _req: &Request<'_>,
    parent: u64,
    name: &OsStr,
    mode: u32,
    _umask: u32,
    flags: i32,
    reply: ReplyCreate,
) {
    println!(
        "create llamado: parent = {parent}, name = {:?}, mode = {mode:#o}, flags = {flags}",
        name
    );

    let name_str = name.to_string_lossy().to_string();

    let mut inner = self.inner.write().unwrap();

    // 1) Verificar que el padre existe y es directorio
    let parent_dir = match inner.directories.get_mut(&parent) {
        Some(d) => d,
        None => {
            reply.error(libc::ENOTDIR);
            return;
        }
    };

    // 2) Verificar que no exista ya una entrada con ese nombre
    if parent_dir.entries.contains_key(&name_str) {
        reply.error(libc::EEXIST);
        return;
    }

    // 3) Reservar un nuevo inodo lógico
    let ino = inner.next_ino;
    inner.next_ino += 1;

    let inode = Inode::file(ino, 0);
    inner.inodes.insert(ino, inode.clone());

    // 4) Agregar la entrada al directorio padre
    parent_dir.entries.insert(name_str.clone(), ino);

    // 5) Inicializar el contenido del archivo vacío
    inner.files.insert(ino, Vec::new());

    // 6-bis) Crear también el inodo en disco (versión mínima)
    {
        let qr_folder = inner.qr_folder.clone();
        let sb = &mut inner.superblock;

        if ino <= sb.max_inodes as u64 {
            // Actualizar contador de inodos libres
            if inner.free_inodes > 0 {
                inner.free_inodes -= 1;
            }
            if sb.free_inodes > 0 {
                sb.free_inodes -= 1;
            }

            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u64;

            let disk_inode = InodeDisk {
                id: ino as u32,
                file_type: 1, // archivo regular
                perm: inode.perm,
                uid: inode.uid,
                gid: inode.gid,
                size: 0,
                atime: now,
                mtime: now,
                ctime: now,
                nlink: 1,
                direct_blocks: [0u32; 12],
                indirect_block: 0,
                double_indirect_block: 0,
                _padding: 0,
            };

            if let Err(e) = write_inode_disk(&qr_folder, sb, ino, &disk_inode) {
                eprintln!("Error al escribir inodo {} en disco: {e:?}", ino);
            }

            if let Err(e) = write_superblock(&qr_folder, sb) {
                eprintln!("Error al actualizar superblock tras crear inodo {}: {e:?}", ino);
            }
        } else {
            eprintln!(
                "Advertencia: ino {} excede max_inodes {}: no se crea inodo en disco",
                ino, sb.max_inodes
            );
        }
    }

    // 6) Construir atributos FUSE y responder
    let attr = inode_to_attr(&inode);
    let ttl = Duration::from_secs(1);
    let fh = 0; // no llevamos manejo especial de file handles

    reply.created(&ttl, &attr, fh, 0, flags as u32);
}


                fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        println!(
            "read llamado: ino = {ino}, fh = {fh}, offset = {offset}, size = {size}, flags = {flags}, lock_owner = {:?}",
            lock_owner
        );

        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }

        // Tomamos lo que necesitamos del estado interno y soltamos el lock
        let (qr_folder, superblock, maybe_data) = {
            let inner = self.inner.read().unwrap();
            (
                inner.qr_folder.clone(),
                inner.superblock,                   // SuperblockDisk: Copy
                inner.files.get(&ino).cloned(),    // copia opcional del buffer en RAM
            )
        };

        // 1) Si tenemos el archivo en memoria, leemos desde RAM (como antes)
        if let Some(data) = maybe_data {
            let offset_usize = offset as usize;

            if offset_usize >= data.len() {
                // Más allá del EOF
                reply.data(&[]);
                return;
            }

            let end = std::cmp::min(offset_usize + size as usize, data.len());
            reply.data(&data[offset_usize..end]);
            return;
        }

        // 2) Si no está en RAM, leemos desde disco usando InodeDisk + bloques
        //    (versión mínima: sólo bloques directos)
        let inode_disk = match load_inode_disk(&qr_folder, &superblock, ino) {
            Ok(inode) => inode,
            Err(e) => {
                eprintln!("Error en read al cargar inodo {ino} desde disco: {e:?}");
                reply.error(libc::EIO);
                return;
            }
        };

        // Si es directorio, no lo tratamos como archivo de datos
        if inode_disk.file_type == 2 {
            reply.error(libc::EISDIR);
            return;
        }

        let file_size = inode_disk.size as i64;
        if offset >= file_size {
            // Más allá del EOF
            reply.data(&[]);
            return;
        }

        let max_len = (file_size - offset) as u32;
        let to_read = std::cmp::min(size, max_len) as usize;

        let block_size = superblock.block_size as i64;
        let start = offset;
        let end = offset + to_read as i64;

        let first_block_idx = (start / block_size) as usize;
        let last_block_idx = ((end - 1) / block_size) as usize;

        let mut result = Vec::with_capacity(to_read);

        for i in first_block_idx..=last_block_idx {
            if i >= inode_disk.direct_blocks.len() {
                break;
            }

            let b = inode_disk.direct_blocks[i];
            if b == 0 {
                // Bloque no asignado: lo tratamos como ceros
                let remaining = to_read - result.len();
                if remaining == 0 {
                    break;
                }
                let zeros = vec![0u8; remaining.min(block_size as usize)];
                result.extend_from_slice(&zeros);
                continue;
            }

            let block_data = match read_fs_block(&qr_folder, b) {
                Ok(buf) => buf,
                Err(e) => {
                    eprintln!("Error leyendo bloque de datos {b} para inodo {ino}: {e:?}");
                    reply.error(libc::EIO);
                    return;
                }
            };

            let block_start = i as i64 * block_size;
            let in_block_start = if i == first_block_idx {
                (start - block_start) as usize
            } else {
                0
            };

            let in_block_end = if i == last_block_idx {
                let end_in_block = (end - block_start) as usize;
                end_in_block.min(block_data.len())
            } else {
                block_data.len()
            };

            if in_block_start < in_block_end && in_block_start < block_data.len() {
                result.extend_from_slice(&block_data[in_block_start..in_block_end]);
            }
        }

        if result.len() > to_read {
            result.truncate(to_read);
        }

        reply.data(&result);
    }

    // write
    fn write(
    &mut self,
    _req: &Request<'_>,
    ino: u64,
    fh: u64,
    offset: i64,
    data: &[u8],
    write_flags: u32,
    flags: i32,
    lock_owner: Option<u64>,
    reply: ReplyWrite,
) {
    println!(
        "write llamado: ino = {ino}, fh = {fh}, offset = {offset}, len = {}, write_flags = {write_flags}, flags = {flags}, lock_owner = {:?}",
        data.len(),
        lock_owner
    );

    if offset < 0 {
        reply.error(libc::EINVAL);
        return;
    }

    let mut inner = self.inner.write().unwrap();

    // Archivo debe existir en memoria
    let buf = match inner.files.get_mut(&ino) {
        Some(b) => b,
        None => {
            reply.error(libc::ENOENT);
            return;
        }
    };

    let offset_usize = offset as usize;
    let needed_len = offset_usize + data.len();

    if buf.len() < needed_len {
        buf.resize(needed_len, 0);
    }

    buf[offset_usize..offset_usize + data.len()].copy_from_slice(data);

    // Actualizar inodo lógico (tamaño y tiempos)
    if let Some(inode) = inner.inodes.get_mut(&ino) {
        let new_size = needed_len as u64;
        if new_size > inode.size {
            inode.size = new_size;
        }
        let now = SystemTime::now();
        inode.mtime = now;
        inode.ctime = now;
    }

        // Persistir versión mínima en disco: un solo bloque directo [0]
    {
        let qr_folder = inner.qr_folder.clone();
        let sb = inner.superblock; // copia

        // Tomamos el contenido completo actual del archivo
        if let Some(full_data) = inner.files.get(&ino) {
            let block_size = sb.block_size as usize;
            let to_write = std::cmp::min(block_size, full_data.len());
            let data = &full_data[..to_write];

            // Cargar el inodo de disco (puede estar en cero si nunca se inicializó bien)
            let mut disk_inode = match load_inode_disk(&qr_folder, &sb, ino) {
                Ok(inode) => inode,
                Err(e) => {
                    eprintln!("Error al cargar inodo {} desde disco en write: {e:?}", ino);
                    // Creamos uno desde cero como fallback
                    InodeDisk {
                        id: ino as u32,
                        file_type: 1,
                        perm: 0o644,
                        uid: 0,
                        gid: 0,
                        size: 0,
                        atime: 0,
                        mtime: 0,
                        ctime: 0,
                        nlink: 1,
                        direct_blocks: [0u32; 12],
                        indirect_block: 0,
                        double_indirect_block: 0,
                        _padding: 0,
                    }
                }
            };

            // Si no tiene bloque de datos asignado en direct_blocks[0], lo asignamos ahora
            if disk_inode.direct_blocks[0] == 0 {
                match alloc_block(&mut inner) {
                    Ok(b) => {
                        disk_inode.direct_blocks[0] = b;
                    }
                    Err(e) => {
                        eprintln!("Sin bloques libres para archivo {}: {e:?}", ino);
                        // No podemos persistir, pero el write en memoria ya se hizo
                        reply.written(data.len() as u32);
                        return;
                    }
                }
            }

            let data_block = disk_inode.direct_blocks[0];

            if let Err(e) = write_fs_block(&qr_folder, data_block, data) {
                eprintln!(
                    "Error al escribir bloque de datos {} para inodo {}: {e:?}",
                    data_block, ino
                );
            } else {
                // Actualizamos tamaño en disco y tiempos básicos
                disk_inode.size = to_write as u64;

                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u64;
                disk_inode.mtime = now;
                disk_inode.ctime = now;

                if let Err(e) = write_inode_disk(&qr_folder, &sb, ino, &disk_inode) {
                    eprintln!("Error al actualizar inodo {} en disco: {e:?}", ino);
                }
            }
        }
    }


    reply.written(data.len() as u32);
}

}
