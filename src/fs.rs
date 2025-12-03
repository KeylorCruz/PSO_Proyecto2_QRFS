use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use anyhow::Result;
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
    Request,
    
};
use libc::ENOENT;

use crate::dir;

pub const ROOT_INO: u64 = 1;

// ----------------- Inodos y directorios en memoria -----------------

#[derive(Debug)]
pub struct Inode {
    pub ino: u64,
    pub kind: FileType,
    pub size: u64,
    pub nlink: u32,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
}

impl Inode {
    pub fn dir(ino: u64) -> Self {
        let now = SystemTime::now();
        Self {
            ino,
            kind: FileType::Directory,
            size: 0,
            nlink: 2,
            atime: now,
            mtime: now,
            ctime: now,
        }
    }

    pub fn file(ino: u64, size: u64) -> Self {
        let now = SystemTime::now();
        Self {
            ino,
            kind: FileType::RegularFile,
            size,
            nlink: 1,
            atime: now,
            mtime: now,
            ctime: now,
        }
    }
}

#[derive(Debug)]
pub struct DirNode {
    pub entries: HashMap<String, u64>, // nombre -> ino
    pub parent: u64,                   // ino del padre
}

pub fn inode_to_attr(inode: &Inode) -> FileAttr {
    let perm = match inode.kind {
        FileType::Directory => 0o755,
        FileType::RegularFile => 0o644,
        _ => 0o644,
    };

    FileAttr {
        ino: inode.ino,
        size: inode.size,
        blocks: 0,
        atime: inode.atime,
        mtime: inode.mtime,
        ctime: inode.ctime,
        crtime: inode.ctime,
        kind: inode.kind,
        perm,
        nlink: inode.nlink,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

// ----------------- Estado del FS -----------------

pub struct QrfsInner {
    pub qr_folder: PathBuf,
    pub inodes: HashMap<u64, Inode>,
    pub directories: HashMap<u64, DirNode>,
    pub next_ino: u64,
}

#[derive(Clone)]
pub struct QrfsFilesystem {
    inner: Arc<RwLock<QrfsInner>>,
}

impl QrfsFilesystem {
    pub fn mount_from_folder(
        qr_folder: &Path,
        _passphrase: Option<String>,
        _start_qr: Option<PathBuf>,
    ) -> Result<Self> {
        // Por ahora ignoramos QR y armamos un FS de prueba en memoria.

        let mut inodes = HashMap::new();
        let mut directories = HashMap::new();

        // 1. Inodo raíz
        let root_inode = Inode::dir(ROOT_INO);
        inodes.insert(ROOT_INO, root_inode);

        let mut root_dir = DirNode {
            entries: HashMap::new(),
            parent: ROOT_INO,
        };

        // 2. Inodo de archivo demo.txt
        let demo_file_ino = 2;
        inodes.insert(demo_file_ino, Inode::file(demo_file_ino, 0));

        // 3. Inodo de subdirectorio subdir/
        let subdir_ino = 3;
        inodes.insert(subdir_ino, Inode::dir(subdir_ino));

        directories.insert(
            subdir_ino,
            DirNode {
                entries: HashMap::new(),
                parent: ROOT_INO,
            },
        );

        // 4. Agregar entradas al directorio raíz
        root_dir
            .entries
            .insert("demo.txt".to_string(), demo_file_ino);
        root_dir.entries.insert("subdir".to_string(), subdir_ino);

        directories.insert(ROOT_INO, root_dir);

        let inner = QrfsInner {
            qr_folder: qr_folder.to_path_buf(),
            inodes,
            directories,
            next_ino: 4,
        };

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })
    }
    pub fn run(self, mountpoint: PathBuf) -> Result<()> {
        let options = vec![
            MountOption::FSName("qrfs".to_string()),
            MountOption::AutoUnmount,
            MountOption::RW, // read write
        ];

        fuser::mount2(self, &mountpoint, &options)?;
        Ok(())
    }

}

// ----------------- Implementación FUSE -----------------

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

        let fh = 0;
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

    // mkdir
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

    // rmdir
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

    // rename
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

    // open
    fn open(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        flags: i32,
        reply: ReplyOpen,
    ) {
        println!("open llamado: ino = {ino}, flags = {flags}");

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

        // Nota: si luego quieren hacerlo real, aquí es donde:
        // - Reservarían un nuevo inodo tipo archivo
        // - Lo agregarían al directorio padre
        // - Inicializarían el mapa de datos del archivo
        // - Construirían un FileAttr y usarían reply.created(...)
    }

    // read
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

        // Versión mínima:
        // Respondemos EOF (cero bytes). No leemos nada real todavía.
        reply.data(&[]);
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

        // Versión mínima:
        // Aceptamos los datos "de mentira": decimos que se escribieron,
        // pero todavía no los guardamos en ningún lado.
        reply.written(data.len() as u32);
    }

}

