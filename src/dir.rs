// src/dir.rs
use std::collections::HashMap;
use std::ffi::OsStr;

use fuser::{FileAttr, FileType, FUSE_ROOT_ID};
use libc::{ENOTDIR, ENOENT, ENOTEMPTY};
use thiserror::Error;

use crate::fs::QrfsInner;

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

pub fn is_directory(_inner: &QrfsInner, ino: u64) -> bool {
    ino == FUSE_ROOT_ID
}


pub fn list_directory(_inner: &QrfsInner, ino: u64) -> Result<Vec<DirEntry>, DirError> {
    if ino != FUSE_ROOT_ID {
        return Err(DirError::NotDirectory);
    }
    Ok(Vec::new()) // raíz vacía
}

pub fn parent_inode(_inner: &QrfsInner, ino: u64) -> Option<u64> {
    if ino == FUSE_ROOT_ID {
        Some(FUSE_ROOT_ID)
    } else {
        Some(FUSE_ROOT_ID)
    }
}

pub fn create_directory(
    inner: &mut QrfsInner,
    parent: u64,
    _name: &OsStr,
    _mode: u32,
) -> Result<FileAttr, DirError> {
    // Paso 1: validar que parent sea dir
    if !is_directory(inner, parent) {
        return Err(DirError::NotDirectory);
    }

    // De momento no soportamos crear directorios en QRFS.
    Err(DirError::NotSupported)
}


pub fn remove_directory(
    _inner: &mut QrfsInner,
    _parent: u64,
    _name: &OsStr,
) -> Result<(), DirError> {
    // De momento no soportamos borrar directorios en QRFS.
    Err(DirError::NotSupported)
}


pub fn rename_entry(
    _inner: &mut QrfsInner,
    _parent: u64,
    _name: &OsStr,
    _newparent: u64,
    _newname: &OsStr,
) -> Result<(), DirError> {
    // De momento no soportamos renombrar/mover entradas en QRFS.
    Err(DirError::NotSupported)
}

