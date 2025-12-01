// src/fs.rs
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use fuser::{
    Filesystem,
    Request,
    ReplyEntry,
    ReplyEmpty,
    ReplyDirectory,
    ReplyAttr,   
    FileAttr, 
    FileType,
    MountOption,
};
use libc::ENOENT;

use crate::dir; // tu módulo

pub struct QrfsInner {
    pub qr_folder: PathBuf,
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


impl QrfsFilesystem {
    pub fn mount_from_folder(
        qr_folder: &Path,
        passphrase: Option<String>,
        start_qr: Option<PathBuf>,
    ) -> Result<Self> {
        // TODO:
        // - Escanear carpeta de QRs
        // - Encontrar firma (superblock) usando passphrase si aplica
        // - Cargar estructuras del FS en memoria (inodos, directorios...)
        let inner = QrfsInner {
            qr_folder: qr_folder.to_path_buf(),
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

        // Solo soportamos el directorio raíz sin hijos.
        if parent != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        // Por ahora, no tenemos archivos ni subdirectorios reales.
        // Mapear nombres a inodos.
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
        println!("opendir llamado");
        // Aquí normalmente solo validás que ino sea un directorio
        let inner = self.inner.read().unwrap();
        if !dir::is_directory(&inner, ino) {
            reply.error(libc::ENOTDIR);
            return;
        }

        let fh = 0; // file handle dummy
        reply.opened(fh, 0);
    }

    //getattr
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

    // readdir
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        println!("readdir llamado: ino = {ino}, offset = {offset}");
        let inner = self.inner.read().unwrap();

        // Obtener lista de entradas del módulo dir.rs
        let entries = match dir::list_directory(&inner, ino) {
            Ok(e) => e,
            Err(_) => {
                reply.error(ENOENT);
                return;
            }
        };

        // offset indica desde cuál entrada continuar
        let mut offset_i = offset as usize;

        // Siempre incluir "." y ".." al inicio (offset 0 y 1)
        if offset_i == 0 {
            let full = reply.add(ino, 1, FileType::Directory, ".");
            if full {
                reply.ok();
                return;
            }
            offset_i = 1;
        }
        if offset_i == 1 {
            let parent = dir::parent_inode(&inner, ino).unwrap_or(ino);
            let full = reply.add(parent, 2, FileType::Directory, "..");
            if full {
                reply.ok();
                return;
            }
            offset_i = 2;
        }

        // Resto de entradas
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
            Ok(attr) => reply.entry(&std::time::Duration::from_secs(1), &attr, 0),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // rmdir
    fn rmdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: ReplyEmpty,
    ) {
        println!("rmdir llamado: parent = {parent}, name = {:?}", name);
        let mut inner = self.inner.write().unwrap();
        match dir::remove_directory(&mut inner, parent, name) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.as_errno()),
        }
    }

    // rename (para archivos y dir)
    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        newparent: u64,
        newname: &std::ffi::OsStr,
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
}

