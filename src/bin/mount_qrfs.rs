// src/bin/mount_qrfs.rs
use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use qrfs::QrfsFilesystem; // struct que vive en la librería

fn main() -> Result<()> {
    // 1. Leer argumentos de la línea de comandos
    //    Esperamos: mount_qrfs qrfolder/ mountpoint/
    let mut args = env::args().skip(1); // saltamos el nombre del binario

    let qr_folder = args
        .next()
        .map(PathBuf::from)
        .context("Uso: mount_qrfs qrfolder/ mountpoint/")?;

    let mountpoint = args
        .next()
        .map(PathBuf::from)
        .context("Uso: mount_qrfs qrfolder/ mountpoint/")?;

    // (Opcional) 3er argumento: archivo de inicio específico del FS
    let start_qr = args.next().map(PathBuf::from);

    // 2. Passphrase (opcional). Por ahora la dejamos en None.
    let passphrase = None::<String>;

    // 3. Construir la estructura del FS desde la carpeta de QRs.
    //    Este método está implementado en la librería (fs.rs)
    let fs = QrfsFilesystem::mount_from_folder(&qr_folder, passphrase, start_qr)
        .context("Error al inicializar QRFS")?;

    // 4. Montar el filesystem con FUSE en mountpoint
    fs.run(mountpoint)
}
