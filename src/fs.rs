// src/fs.rs

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
    Filesystem,
    Request,
    ReplyEntry,
    ReplyEmpty,
    ReplyDirectory,
    ReplyAttr,
    ReplyStatfs,
    FileAttr,
    FileType,
    MountOption,
};
use libc::ENOENT;

use crate::dir; // módulo de manejo de directorios (stubs por ahora)

// -------------------- Constantes de QRFS --------------------

pub const QRFS_BLOCK_SIZE: u32 = 1024;
pub const QRFS_MAGIC: u32   = 0x5152_4653; // "QRFS"
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

// -------------------- Estado en memoria del FS --------------------

pub struct QrfsInner {
    pub qr_folder: PathBuf,
    pub superblock: SuperblockDisk,
    pub free_blocks: u32,
    pub free_inodes: u32,
    // luego: tabla de inodos en memoria, directorios, etc.
    // acá van superblock, inodos, tablas de directorios, etc.
    // Ejemplos:
    // pub inodes: Vec<Inode>,
    // pub directories: HashMap<InodeId, Directory>,
}

#[derive(Clone)]
pub struct QrfsFilesystem {
    inner: Arc<RwLock<QrfsInner>>,
}

const ROOT_INO: u64 = 1;

fn root_attr() -> FileAttr {
    let now = SystemTime::now();

    FileAttr {
        ino: ROOT_INO,
        size: 0,
        blocks: 0,
        atime: now,
        mtime: now,
        ctime: now,
        crtime: now,
        kind: FileType::Directory,
        perm: 0o755,   // drwxr-xr-x
        nlink: 2,
        uid: 0,        // dueño root (no afecta mucho para probar)
        gid: 0,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

// -------------------- Implementación de QrfsFilesystem --------------------

impl QrfsFilesystem {
    /// Construye el estado interno del FS a partir de una carpeta con QRs.
    /// - Lee el primer archivo como bloque 0 (superblock)
    /// - Valida magic y versión
    /// - Deja en memoria el superblock y contadores de bloques/inodos libres
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
                "La carpeta {:?} no contiene archivos para montar QRFS",
                qr_folder
            ));
        }

        // Por ahora: asumimos que el bloque 0 (superblock) es el primer archivo ordenado.
        let superblock_path = entries[0].clone();

        // 2. Leer el bloque 0 completo
        let mut file = File::open(&superblock_path)
            .with_context(|| format!("No se pudo abrir el archivo {:?}", superblock_path))?;

        let mut buf = vec![0u8; QRFS_BLOCK_SIZE as usize];
        file.read_exact(&mut buf)
            .with_context(|| format!("No se pudo leer el bloque 0 desde {:?}", superblock_path))?;

        // 3. Deserializar el SuperblockDisk
        if buf.len() < mem::size_of::<SuperblockDisk>() {
            return Err(anyhow::anyhow!(
                "El bloque 0 es demasiado pequeño para contener un SuperblockDisk"
            ));
        }

        let superblock: SuperblockDisk = unsafe {
            // Interpretamos los primeros bytes como un SuperblockDisk.
            std::ptr::read(buf.as_ptr() as *const SuperblockDisk)
        };

        // 4. Validar que esto parece un QRFS
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

        // 5. Construir el estado interno
        let inner = QrfsInner {
            qr_folder: qr_folder.to_path_buf(),
            free_blocks: superblock.free_blocks,
            free_inodes: superblock.free_inodes,
            superblock,
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

// -------------------- Implementación de Filesystem (FUSE) --------------------

impl Filesystem for QrfsFilesystem {
    // lookup
    fn lookup(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: ReplyEntry,
    ) {
        println!("lookup llamado: parent = {parent}, name = {:?}", name);

        // Solo soportamos el directorio raíz sin hijos por ahora.
        if parent != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        // En esta versión base, la raíz no tiene entradas.
        // Si quisieras soportar, por ejemplo, "info.txt" fijo:
        //
        // if name == OsStr::new("info.txt") { ... }
        //
        // Por ahora: no encontrado.
        reply.error(ENOENT);
    }

    // opendir
    fn opendir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _flags: i32,
        reply: fuser::ReplyOpen,
    ) {
        println!("opendir llamado: ino = {ino}");

        // Solo se puede abrir el root.
        if ino != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        let fh = 0; // file handle dummy
        reply.opened(fh, 0);
    }

    // getattr
    fn getattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        reply: ReplyAttr,
    ) {
        println!("getattr llamado: ino = {ino}");

        if ino == ROOT_INO {
            let attr = root_attr();
            let ttl = Duration::from_secs(1);
            reply.attr(&ttl, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    // access
    fn access(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mask: i32,
        reply: ReplyEmpty,
    ) {
        println!("access llamado: ino = {ino}");

        // Primera versión: solo reconocemos el root.
        if ino == ROOT_INO {
            reply.ok();
        } else {
            reply.error(ENOENT);
        }
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

    // readdir (delegado a dir.rs)
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        println!("readdir llamado: ino = {ino}, fh = {fh}, offset = {offset}");

        // Solo soportamos listados en el root por ahora.
        let inner = self.inner.read().unwrap();
        let entries_result = dir::list_directory(&inner, ino);

        let entries = match entries_result {
            Ok(v) => v,
            Err(e) => {
                reply.error(e.as_errno());
                return;
            }
        };

        let mut offset_i = offset;
        if offset_i == 0 {
            // Entradas "." y ".."
            let full = reply.add(ino, 1, FileType::Directory, ".");
            if full {
                reply.ok();
                return;
            }
            let parent = dir::parent_inode(&inner, ino).unwrap_or(ino);
            let full = reply.add(parent, 2, FileType::Directory, "..");
            if full {
                reply.ok();
                return;
            }
            offset_i = 2;
        }

        // Resto de entradas (offset >= 2)
        let start = (offset_i - 2) as usize;
        for (i, e) in entries.iter().enumerate().skip(start) {
            let next_offset = (i + 3) as i64;
            let full = reply.add(e.ino, next_offset, e.file_type, &e.name);
            if full {
                break;
            }
        }

        reply.ok();
    }

    // fsync: en el futuro debería forzar flush desde memoria a los QRs.
    fn fsync(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        println!("fsync llamado: ino = {ino}");
        // Más adelante: forzar flush de buffers al QR físico.
        reply.ok();
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
        println!("rename llamado: parent = {parent}, name = {:?}", name);
        let mut inner = self.inner.write().unwrap();
        match dir::rename_entry(&mut inner, parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // NOTA: create/open/read/write usan los defaults de FUSE (ENOSYS) por ahora.
    // Cuando quieras implementar esos requerimientos del enunciado,
    // se agregan aquí las funciones correspondientes.
}