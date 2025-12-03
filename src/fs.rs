use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::mem;

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
        _start_qr: Option<PathBuf>,
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

        // 4. Construir e inicializar el estado interno en memoria
        let mut inodes = HashMap::new();
        let mut directories = HashMap::new();

        // Inodo lógico para el root (ino = 1)
        let root_ino = ROOT_INO;
        let root_inode = Inode::dir(root_ino);
        inodes.insert(root_ino, root_inode);

        // Directorio raíz: parent = él mismo, sin entradas de hijos por ahora.
        let root_dir = Directory {
            parent: root_ino,
            entries: HashMap::new(),
        };
        directories.insert(root_ino, root_dir);

        let inner = QrfsInner {
            qr_folder: qr_folder.to_path_buf(),
            superblock,
            free_blocks: superblock.free_blocks,
            free_inodes: superblock.free_inodes,
            inodes,
            directories,
            next_ino: root_ino + 1,
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

        // Versión mínima:
        // Por ahora NO creamos estructuras reales en disco/memoria.
        // Reportamos "operación no implementada" para que el kernel lo sepa.
        reply.error(libc::ENOSYS);

        // TODO:
        // - Reservar un nuevo inodo tipo archivo
        // - Agregarlo al directorio padre
        // - Inicializar el mapa de datos del archivo
        // - Construir un FileAttr y usar reply.created(...)
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

        // Tomamos la info necesaria y soltamos el lock lo antes posible
        let (qr_folder, superblock) = {
            let inner = self.inner.read().unwrap();
            (inner.qr_folder.clone(), inner.superblock)
        };

        // Cargar el inodo desde disco
        let inode_disk = match load_inode_disk(&qr_folder, &superblock, ino) {
            Ok(inode) => inode,
            Err(e) => {
                eprintln!("Error en read al cargar inodo {ino}: {e:?}");
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

        let _ = (ino, fh, offset); // por ahora no usamos estos

        // Versión mínima:
        // Aceptamos los datos "de mentira": decimos que se escribieron,
        // pero todavía no los guardamos en ningún lado.
        reply.written(data.len() as u32);
    }
}
