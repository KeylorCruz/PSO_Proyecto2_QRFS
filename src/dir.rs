// src/dir.rs
use std::collections::HashMap;
use std::ffi::OsStr;

use fuser::{FileAttr, FileType};
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
    // etc
}

impl DirError {
    pub fn as_errno(&self) -> i32 {
        match self {
            DirError::NotDirectory => ENOTDIR,
            DirError::NotFound => ENOENT,
            DirError::NotEmpty => ENOTEMPTY,
            DirError::NoSpace => libc::ENOSPC,
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
    // TODO: mirar el inodo con id = ino y revisar su tipo
    // inner.inodes[ino as usize].is_dir()
    true
}

pub fn list_directory(inner: &QrfsInner, ino: u64) -> Result<Vec<DirEntry>, DirError> {
    // TODO: devolver las entradas de ese directorio desde tu tabla
    // Ejemplo ficticio:
    //
    // let dir = inner.directories.get(&ino).ok_or(DirError::NotDirectory)?;
    // Ok(dir.entries.iter().map(|e| ... ).collect())

    Ok(Vec::new())
}

pub fn parent_inode(inner: &QrfsInner, ino: u64) -> Option<u64> {
    // TODO: tabla de padres o campo en el inodo
    Some(ino)
}

pub fn create_directory(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
    mode: u32,
) -> Result<FileAttr, DirError> {
    // Paso 1: validar que parent sea dir
    if !is_directory(inner, parent) {
        return Err(DirError::NotDirectory);
    }

    // Paso 2: revisar que no exista el nombre
    // Paso 3: reservar nuevo inodo tipo directorio
    // Paso 4: agregar entrada en el directorio padre
    // Paso 5: devolver FileAttr del nuevo inodo

    todo!("implementar create_directory");
}

pub fn remove_directory(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
) -> Result<(), DirError> {
    // Paso 1: buscar entrada (obtengo ino del directorio a borrar)
    // Paso 2: verificar que esté vacío
    // Paso 3: borrar entrada del padre
    // Paso 4: marcar inodo como libre

    todo!("implementar remove_directory");
}

pub fn rename_entry(
    inner: &mut QrfsInner,
    parent: u64,
    name: &OsStr,
    newparent: u64,
    newname: &OsStr,
) -> Result<(), DirError> {
    // Paso 1: buscar entrada en (parent, name)
    // Paso 2: remover del padre original
    // Paso 3: insertar entrada en newparent con newname

    todo!("implementar rename_entry");
}
