// src/fs.rs
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::ffi::OsStr;

use anyhow::Result;
use fuser::{
    Filesystem,
    Request,
    ReplyEntry,
    ReplyEmpty,
    ReplyDirectory,
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
            MountOption::RW, // o RO si quieren solo lectura
        ];

        fuser::mount2(self, &mountpoint, &options)?;
        Ok(())
    }

}

impl Filesystem for QrfsFilesystem {
    // otras funciones (getattr, read, write) las pueden implementar en equipo.

    // ---------- TU PARTE: DIRECTORIOS ----------

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

        // Podrías guardar un handle, pero con fuser no es obligatorio:
        let fh = 0; // file handle dummy
        reply.opened(fh, 0);
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

        // 1. Obtener lista de entradas de tu módulo dir.rs
        let entries = match dir::list_directory(&inner, ino) {
            Ok(e) => e,
            Err(_) => {
                reply.error(ENOENT);
                return;
            }
        };

        // 2. offset indica desde cuál entrada continuar
        let mut offset_i = offset as usize;

        // 3. Siempre incluir "." y ".." al inicio (offset 0 y 1)
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

        // 4. Resto de entradas
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
        println!("mkdir llamado: parent = {parent}, name = {:?}", name);
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

