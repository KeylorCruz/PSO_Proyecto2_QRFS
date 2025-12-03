use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::mem;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use qrfs::{SuperblockDisk, InodeDisk, QRFS_BLOCK_SIZE, QRFS_MAGIC, QRFS_VERSION};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let qr_folder = args
        .next()
        .map(PathBuf::from)
        .context("Uso: fsck.qrfs qrfolder/")?;

    if args.next().is_some() {
        return Err(anyhow!("Uso: fsck.qrfs qrfolder/ (solo un argumento)"));
    }

    // 1. Listar archivos QR
    let mut entries: Vec<PathBuf> = fs::read_dir(&qr_folder)
        .with_context(|| format!("No se pudo leer el directorio {:?}", qr_folder))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();

    entries.sort();

    if entries.is_empty() {
        return Err(anyhow!(
            "La carpeta {:?} no contiene archivos para QRFS",
            qr_folder
        ));
    }

    // 2. Leer superblock (bloque 0)
    let sb = read_superblock(&entries[0])?;

    println!("== fsck.qrfs ==\n");
    println!("Superblock:");
    println!("  magic           = 0x{:08x}", sb.magic);
    println!("  version         = {}", sb.version);
    println!("  block_size      = {}", sb.block_size);
    println!("  total_blocks    = {}", sb.total_blocks);
    println!("  inode_table     = start {}, blocks {}",
             sb.inode_table_start, sb.inode_table_blocks);
    println!("  free_bitmap     = start {}, blocks {}",
             sb.free_bitmap_start, sb.free_bitmap_blocks);
    println!("  data_blocks     = start {}", sb.data_blocks_start);
    println!("  max_inodes      = {}", sb.max_inodes);
    println!("  root_inode      = {}", sb.root_inode);
    println!("  free_blocks     = {}", sb.free_blocks);
    println!("  free_inodes     = {}", sb.free_inodes);
    println!();

    // 3. Chequeos básicos del superblock
    let mut errors = 0usize;

    if sb.magic != QRFS_MAGIC {
        println!("ERROR: magic incorrecto (leído = 0x{:08x}, esperado = 0x{:08x})",
                 sb.magic, QRFS_MAGIC);
        errors += 1;
    }

    if sb.version != QRFS_VERSION {
        println!("ERROR: versión de FS no soportada (leída = {}, esperada = {})",
                 sb.version, QRFS_VERSION);
        errors += 1;
    }

    if sb.block_size != QRFS_BLOCK_SIZE {
        println!("ERROR: block_size incorrecto (leído = {}, esperado = {})",
                 sb.block_size, QRFS_BLOCK_SIZE);
        errors += 1;
    }

    if sb.total_blocks == 0 || sb.total_blocks as usize > entries.len() {
        println!(
            "ERROR: total_blocks = {} no coincide con cantidad de archivos = {}",
            sb.total_blocks,
            entries.len()
        );
        errors += 1;
    }

    let max_block = sb.total_blocks.checked_sub(1).unwrap_or(0);
    if sb.inode_table_start == 0 {
        println!("ERROR: inode_table_start no puede ser 0 (reservado para superblock).");
        errors += 1;
    }
    if sb.inode_table_start > max_block
        || sb.inode_table_start + sb.inode_table_blocks > sb.total_blocks
    {
        println!("ERROR: rango de tabla de inodos inválido.");
        errors += 1;
    }

    if sb.free_bitmap_start > max_block
        || sb.free_bitmap_start + sb.free_bitmap_blocks > sb.total_blocks
    {
        println!("ERROR: rango de bitmap inválido.");
        errors += 1;
    }

    if sb.data_blocks_start > max_block {
        println!("ERROR: data_blocks_start fuera de rango.");
        errors += 1;
    }

    if sb.root_inode == 0 || sb.root_inode > sb.max_inodes {
        println!("ERROR: root_inode fuera de rango (root_inode = {}, max_inodes = {}).",
                 sb.root_inode, sb.max_inodes);
        errors += 1;
    }

    // 4. Leer tabla de inodos y bitmap
    let inodes = read_inode_table(&entries, &sb)?;
    let bitmap = read_bitmap(&entries, &sb)?;

    // 5. Chequeos sobre inodos
    println!("\nChequeando inodos...");
    let inode_size = mem::size_of::<InodeDisk>();
    println!("  inode_size = {} bytes, max_inodes = {}", inode_size, sb.max_inodes);

    if inodes.len() as u32 != sb.max_inodes {
        println!(
            "ADVERTENCIA: inodes.len() = {}, max_inodes = {} (no coinciden).",
            inodes.len(),
            sb.max_inodes
        );
        // no necesariamente lo tratamos como error fatal
    }

    // Verificar root
    if let Some(root) = inodes.get((sb.root_inode - 1) as usize) {
        if root.file_type != 2 {
            println!(
                "ERROR: root_inode no es un directorio (file_type = {}).",
                root.file_type
            );
            errors += 1;
        }
    } else {
        println!("ERROR: root_inode fuera del vector de inodos.");
        errors += 1;
    }

    // Pequeño conteo: inodos "activos"
    let mut used_inodes = 0u32;
    for inode in &inodes {
        if inode.id != 0 && inode.file_type != 0 {
            used_inodes += 1;
        }
    }

    if used_inodes + sb.free_inodes != sb.max_inodes {
        println!(
            "ADVERTENCIA: used_inodes ({}) + free_inodes ({}) != max_inodes ({}).",
            used_inodes, sb.free_inodes, sb.max_inodes
        );
    } else {
        println!(
            "  OK: used_inodes ({}) + free_inodes ({}) == max_inodes ({}).",
            used_inodes, sb.free_inodes, sb.max_inodes
        );
    }

    // 6. Chequeos sobre bitmap
    println!("\nChequeando bitmap...");

    let total_blocks = sb.total_blocks as usize;
    if bitmap.len() * 8 < total_blocks {
        println!(
            "ERROR: bitmap demasiado pequeño ({:?} bits) para {} bloques.",
            bitmap.len() * 8,
            total_blocks
        );
        errors += 1;
    } else {
        // Contar bloques marcados como usados según bitmap
        let mut used_bits = 0usize;
        for b in 0..total_blocks {
            if bit_is_set(&bitmap, b as u32) {
                used_bits += 1;
            }
        }
        let meta_blocks = sb.data_blocks_start as usize;
        let data_blocks = total_blocks - meta_blocks;
        let expected_used = meta_blocks + (data_blocks - sb.free_blocks as usize);

        println!(
            "  bitmap: used_bits = {}, meta_blocks = {}, data_blocks = {}, free_blocks = {}",
            used_bits, meta_blocks, data_blocks, sb.free_blocks
        );

        if used_bits != expected_used {
            println!(
                "ADVERTENCIA: used_bits ({}) != meta_blocks + usados según superblock ({}).",
                used_bits, expected_used
            );
        } else {
            println!("  OK: bitmap consistente con contadores del superblock.");
        }
    }

    println!("\n== Resultado fsck ==");
    if errors == 0 {
        println!("  No se encontraron errores críticos (pueden existir advertencias).");
        Ok(())
    } else {
        println!("  Se encontraron {} error(es) crítico(s).", errors);
        Err(anyhow!("fsck detectó inconsistencias."))
    }
}

/// Lee y deserializa el SuperblockDisk desde el archivo dado.
fn read_superblock(path: &PathBuf) -> Result<SuperblockDisk> {
    let mut file = File::open(path)
        .with_context(|| format!("No se pudo abrir el archivo {:?}", path))?;

    let mut buf = vec![0u8; QRFS_BLOCK_SIZE as usize];
    file.read_exact(&mut buf)
        .with_context(|| format!("No se pudo leer el bloque 0 desde {:?}", path))?;

    if buf.len() < mem::size_of::<SuperblockDisk>() {
        return Err(anyhow!(
            "El bloque 0 es demasiado pequeño para contener un SuperblockDisk"
        ));
    }

    let superblock: SuperblockDisk = unsafe {
        std::ptr::read(buf.as_ptr() as *const SuperblockDisk)
    };

    Ok(superblock)
}

/// Lee la tabla de inodos completa desde los bloques reservados.
fn read_inode_table(entries: &[PathBuf], sb: &SuperblockDisk) -> Result<Vec<InodeDisk>> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let inode_size = mem::size_of::<InodeDisk>();
    let total_bytes = (sb.max_inodes as usize) * inode_size;

    let mut buf = Vec::with_capacity(total_bytes);
    let mut remaining = total_bytes;
    let mut block_index = sb.inode_table_start as usize;

    while remaining > 0 {
        if block_index >= entries.len() {
            return Err(anyhow!(
                "No hay suficientes bloques para leer la tabla de inodos."
            ));
        }
        let mut file = File::open(&entries[block_index])
            .with_context(|| format!("No se pudo abrir {:?}", entries[block_index]))?;
        let mut block_buf = vec![0u8; block_size];
        file.read_exact(&mut block_buf)
            .with_context(|| format!("No se pudo leer bloque de inodos {:?}", entries[block_index]))?;

        let to_copy = remaining.min(block_size);
        buf.extend_from_slice(&block_buf[..to_copy]);

        remaining -= to_copy;
        block_index += 1;
    }

    // Convertir bytes en Vec<InodeDisk>
    let mut inodes = Vec::with_capacity(sb.max_inodes as usize);
    let mut offset = 0;
    while offset + inode_size <= buf.len() {
        let inode: InodeDisk = unsafe {
            std::ptr::read(buf[offset..].as_ptr() as *const InodeDisk)
        };
        inodes.push(inode);
        offset += inode_size;
    }

    Ok(inodes)
}

/// Lee el bitmap desde los bloques reservados.
fn read_bitmap(entries: &[PathBuf], sb: &SuperblockDisk) -> Result<Vec<u8>> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let total_blocks = sb.total_blocks as usize;
    let bitmap_bits = total_blocks;
    let bitmap_bytes = (bitmap_bits + 7) / 8;

    let mut buf = Vec::with_capacity(bitmap_bytes);
    let mut remaining = bitmap_bytes;
    let mut block_index = sb.free_bitmap_start as usize;

    while remaining > 0 {
        if block_index >= entries.len() {
            return Err(anyhow!(
                "No hay suficientes bloques para leer el bitmap."
            ));
        }
        let mut file = File::open(&entries[block_index])
            .with_context(|| format!("No se pudo abrir {:?}", entries[block_index]))?;
        let mut block_buf = vec![0u8; block_size];
        file.read_exact(&mut block_buf)
            .with_context(|| format!("No se pudo leer bloque de bitmap {:?}", entries[block_index]))?;

        let to_copy = remaining.min(block_size);
        buf.extend_from_slice(&block_buf[..to_copy]);

        remaining -= to_copy;
        block_index += 1;
    }

    Ok(buf)
}

/// Devuelve true si el bit correspondiente al bloque `block` está en 1 (usado).
fn bit_is_set(bitmap: &[u8], block: u32) -> bool {
    let idx = block as usize;
    let byte = idx / 8;
    let bit = (idx % 8) as u8;
    if byte >= bitmap.len() {
        return false;
    }
    (bitmap[byte] & (1 << bit)) != 0
}