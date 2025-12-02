use std::ffi::OsStr;

use fuser::{FileAttr, FileType};
use libc::{ENOTDIR, ENOENT, ENOTEMPTY};
use thiserror::Error;

use crate::fs::{inode_to_attr, QrfsInner};

#[derive(Debug, Error)]
pub enum DirError {
    #[error("no es un directorio")]
    NotDirectory,
    #[error("entrada no encontrada")]
    NotFound,
    #[error("directorio no vacío")]
    NotEmpty,
    #[error("espacio insuficiente")]
    NoSpace,
    #[error("operación no soportada")]
    NotSupported,
}


impl DirError {
    pub fn as_errno(&self) -> i32 {
        match self {
            DirError::NotDirectory => ENOTDIR,
            DirError::NotFound => ENOENT,
            DirError::NotEmpty => ENOTEMPTY,
            DirError::NoSpace => libc::ENOSPC,
            DirError::NotSupported => libc::ENOSYS,
        }
    }
}

pub struct DirEntry {
    pub ino: u64,
    pub name: String,
    pub file_type: FileType,
}

// --------- Funciones usadas por Filesystem ---------

pub fn is_directory(inner: &QrfsInner, ino: u64) -> bool {
    if let Some(inode) = inner.inodes.get(&ino) {
        matches!(inode.kind, FileType::Directory)
    } else {
        false
    }
}


pub fn list_directory(inner: &QrfsInner, ino: u64) -> Result<Vec<DirEntry>, DirError> {
    let dir = inner
        .directories
        .get(&ino)
        .ok_or(DirError::NotDirectory)?;

    let mut entries = Vec::new();

    for (name, child_ino) in &dir.entries {
        let inode = inner.inodes.get(child_ino).ok_or(DirError::NotFound)?;
        entries.push(DirEntry {
            ino: *child_ino,
            name: name.clone(),
            file_type: inode.kind,
        });
    }

    Ok(entries)
}

pub fn parent_inode(inner: &QrfsInner, ino: u64) -> Option<u64> {
    if let Some(dir) = inner.directories.get(&ino) {
        Some(dir.parent)
    } else {
        Some(ino)
    }
}

pub fn create_directory(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
    _mode: u32,
) -> Result<FileAttr, DirError> {
    if !is_directory(inner, parent) {
        return Err(DirError::NotDirectory);
    }

    let name_str = name.to_string_lossy().to_string();

    // 1) Revisar existencia SIN mantener un &mut vivo
    {
        let parent_dir = inner
            .directories
            .get(&parent)
            .ok_or(DirError::NotDirectory)?;

        if parent_dir.entries.contains_key(&name_str) {
            return Err(DirError::NotSupported);
        }
    }

    // 2) Reservar nuevo inodo
    let new_ino = inner.next_ino;
    inner.next_ino += 1;

    // Crear inodo directorio
    let inode = crate::fs::Inode::dir(new_ino);
    inner.inodes.insert(new_ino, inode);

    // Crear nodo de directorio vacío
    let new_dir = crate::fs::DirNode {
        entries: Default::default(),
        parent,
    };
    inner.directories.insert(new_ino, new_dir);


    // 3) Agregar entrada al padre
    {
        let parent_dir = inner
            .directories
            .get_mut(&parent)
            .ok_or(DirError::NotDirectory)?;
        parent_dir.entries.insert(name_str, new_ino);
    }

    // 4) Devolver FileAttr
    let attr = {
        let inode = inner.inodes.get(&new_ino).unwrap();
        inode_to_attr(inode)
    };

    Ok(attr)
}


pub fn remove_directory(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
) -> Result<(), DirError> {
    let name_str = name.to_string_lossy().to_string();

    // 1) Obtener el ino del hijo SIN dejar vivo un &mut
    let child_ino = {
        let parent_dir = inner
            .directories
            .get(&parent)
            .ok_or(DirError::NotDirectory)?;

        match parent_dir.entries.get(&name_str) {
            Some(ino) => *ino,
            None => return Err(DirError::NotFound),
        }
    };

    // 2) Verificar que sea directorio
    if !is_directory(inner, child_ino) {
        return Err(DirError::NotDirectory);
    }

    // 3) Verificar que esté vacío
    let child_dir = inner
        .directories
        .get(&child_ino)
        .ok_or(DirError::NotDirectory)?;

    if !child_dir.entries.is_empty() {
        return Err(DirError::NotEmpty);
    }

    // 4) Ahora sí, eliminar del padre
    {
        let parent_dir = inner
            .directories
            .get_mut(&parent)
            .ok_or(DirError::NotDirectory)?;
        parent_dir.entries.remove(&name_str);
    }

    // 5) Eliminar estructuras del hijo
    inner.directories.remove(&child_ino);
    inner.inodes.remove(&child_ino);

    Ok(())
}


pub fn rename_entry(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
    newparent: u64,
    newname: &OsStr,
) -> Result<(), DirError> {
    if !is_directory(inner, parent) || !is_directory(inner, newparent) {
        return Err(DirError::NotDirectory);
    }

    let name_str = name.to_string_lossy().to_string();
    let newname_str = newname.to_string_lossy().to_string();

    // 1) Buscar el inodo del hijo sin mantener vivos dos &mut
    let child_ino = {
        let parent_dir = inner
            .directories
            .get(&parent)
            .ok_or(DirError::NotDirectory)?;

        match parent_dir.entries.get(&name_str) {
            Some(ino) => *ino,
            None => return Err(DirError::NotFound),
        }
    };

    // 2) Sacar del padre original
    {
        let parent_dir = inner
            .directories
            .get_mut(&parent)
            .ok_or(DirError::NotDirectory)?;
        parent_dir.entries.remove(&name_str);
    }

    // 3) Insertar en el nuevo padre
    {
        let newparent_dir = inner
            .directories
            .get_mut(&newparent)
            .ok_or(DirError::NotDirectory)?;
        newparent_dir.entries.insert(newname_str, child_ino);
    }

    // 4) Si es directorio, actualizar su campo parent
    if let Some(child_dir) = inner.directories.get_mut(&child_ino) {
        child_dir.parent = newparent;
    }

    Ok(())
}
