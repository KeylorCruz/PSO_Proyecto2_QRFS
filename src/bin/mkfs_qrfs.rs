use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::mem;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use qrfs::{SuperblockDisk, InodeDisk, QRFS_BLOCK_SIZE, QRFS_MAGIC, QRFS_VERSION};

fn main() -> Result<()> {
    // 1. Leer qrfolder/ desde los argumentos
    let mut args = env::args().skip(1);
    let qr_folder = args
        .next()
        .map(PathBuf::from)
        .context("Uso: mkfs.qrfs qrfolder/")?;

    if args.next().is_some() {
        return Err(anyhow!("Uso: mkfs.qrfs qrfolder/ (solo un argumento)"));
    }

    // 2. Listar y ordenar los archivos QR -> total_blocks
    let mut entries: Vec<PathBuf> = fs::read_dir(&qr_folder)
        .with_context(|| format!("No se pudo leer el directorio {:?}", qr_folder))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();

    entries.sort();

    if entries.is_empty() {
        return Err(anyhow!(
            "La carpeta {:?} no contiene archivos para usar como bloques QR",
            qr_folder
        ));
    }

    let total_blocks = entries.len() as u32;

    // 3. Calcular layout (inode_table_start, free_bitmap_start, etc.)
    let layout = build_layout(total_blocks)?;

    // 4. Inicializar superblock, vector de inodos, bitmap
    let (superblock, inodes, bitmap) = init_fresh_fs(&layout)?;

    // 5. Escribir:
    //    - superblock en el primer archivo (bloque 0)
    //    - tabla de inodos en los siguientes
    //    - bitmap en los que siguen
    //    - rellenar bloques de datos con ceros
    write_superblock(&entries, &superblock)?;
    write_inode_table(&entries, &layout, &inodes)?;
    write_bitmap(&entries, &layout, &bitmap)?;
    zero_data_blocks(&entries, &layout)?;

    println!(
        "mkfs.qrfs: sistema QRFS creado con {} bloques, {} inodos máximos, {} bloques de datos.",
        superblock.total_blocks,
        superblock.max_inodes,
        superblock.total_blocks - superblock.data_blocks_start
    );

    Ok(())
}

/// Estructura auxiliar para el layout calculado.
struct FsLayout {
    total_blocks: u32,
    inode_table_start: u32,
    inode_table_blocks: u32,
    free_bitmap_start: u32,
    free_bitmap_blocks: u32,
    data_blocks_start: u32,
    max_inodes: u32,
}

/// Cálculo del layout básico del filesystem dentro de los bloques QR.
fn build_layout(total_blocks: u32) -> Result<FsLayout> {
    if total_blocks < 3 {
        return Err(anyhow!(
            "Se requieren al menos 3 bloques para crear el filesystem (se tienen {}).",
            total_blocks
        ));
    }

    let block_size = QRFS_BLOCK_SIZE as usize;
    let inode_size = mem::size_of::<InodeDisk>();

    if inode_size == 0 || inode_size > block_size {
        return Err(anyhow!(
            "InodeDisk no cabe en un bloque: inode_size={}, block_size={}",
            inode_size,
            block_size
        ));
    }

    let inodes_per_block = block_size / inode_size;

    // Heurística simple:
    // - Reservar ~10% de los bloques para la tabla de inodos (al menos 1).
    // - El número de inodos es inodes_per_block * inode_table_blocks.
    let mut inode_table_blocks = (total_blocks / 10).max(1);
    if inode_table_blocks > total_blocks - 2 {
        inode_table_blocks = 1;
    }
    let max_inodes = inodes_per_block as u32 * inode_table_blocks;

    // Bitmap: 1 bit por bloque.
    let bitmap_bits = total_blocks as usize;
    let bitmap_bytes = (bitmap_bits + 7) / 8;
    let free_bitmap_blocks =
        ((bitmap_bytes as u32) + QRFS_BLOCK_SIZE - 1) / QRFS_BLOCK_SIZE;

    let inode_table_start = 1;
    let free_bitmap_start = inode_table_start + inode_table_blocks;
    let data_blocks_start = free_bitmap_start + free_bitmap_blocks;

    if data_blocks_start >= total_blocks {
        return Err(anyhow!(
            "No hay espacio para bloques de datos: total_blocks={}, data_blocks_start={}",
            total_blocks,
            data_blocks_start
        ));
    }

    Ok(FsLayout {
        total_blocks,
        inode_table_start,
        inode_table_blocks,
        free_bitmap_start,
        free_bitmap_blocks,
        data_blocks_start,
        max_inodes,
    })
}

/// Inicializa un filesystem vacío: superblock, inodos (incluyendo root) y bitmap.
fn init_fresh_fs(layout: &FsLayout) -> Result<(SuperblockDisk, Vec<InodeDisk>, Vec<u8>)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u64;

    let data_blocks = layout.total_blocks - layout.data_blocks_start;

    let superblock = SuperblockDisk {
        magic: QRFS_MAGIC,
        version: QRFS_VERSION,
        block_size: QRFS_BLOCK_SIZE,
        total_blocks: layout.total_blocks,
        inode_table_start: layout.inode_table_start,
        inode_table_blocks: layout.inode_table_blocks,
        free_bitmap_start: layout.free_bitmap_start,
        free_bitmap_blocks: layout.free_bitmap_blocks,
        data_blocks_start: layout.data_blocks_start,
        max_inodes: layout.max_inodes,
        root_inode: 1,
        free_blocks: data_blocks,
        free_inodes: layout.max_inodes.checked_sub(1).unwrap_or(0),
        reserved: [0u8; 64],
    };

    // Crear vector de inodos vacíos.
    let mut inodes = vec![
        InodeDisk {
            id: 0,
            file_type: 0,
            perm: 0,
            uid: 0,
            gid: 0,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            nlink: 0,
            direct_blocks: [0u32; 12],
            indirect_block: 0,
            double_indirect_block: 0,
            _padding: 0,
        };
        layout.max_inodes as usize
    ];

    // Inodo 1 = directorio raíz
    if !inodes.is_empty() {
        inodes[0] = InodeDisk {
            id: 1,
            file_type: 2, // 2 = directorio (convención simple)
            perm: 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            atime: now,
            mtime: now,
            ctime: now,
            nlink: 2, // "." y ".."
            direct_blocks: [0u32; 12],
            indirect_block: 0,
            double_indirect_block: 0,
            _padding: 0,
        };
    }

    // Bitmap: 1 bit por bloque, 1 = usado, 0 = libre.
    let bitmap_bits = layout.total_blocks as usize;
    let bitmap_bytes = (bitmap_bits + 7) / 8;
    let mut bitmap = vec![0u8; bitmap_bytes];

    // Marcar como usados todos los bloques de metadata:
    // [0 .. data_blocks_start)
    for b in 0..layout.data_blocks_start {
        let idx = b as usize;
        let byte = idx / 8;
        let bit = (idx % 8) as u8;
        bitmap[byte] |= 1 << bit;
    }

    Ok((superblock, inodes, bitmap))
}

/// Escribe el superblock en el bloque 0.
fn write_superblock(entries: &[PathBuf], sb: &SuperblockDisk) -> Result<()> {
    let data = struct_to_bytes(sb);
    write_block(&entries[0], &data)
}

/// Escribe la tabla de inodos a partir de inode_table_start.
fn write_inode_table(
    entries: &[PathBuf],
    layout: &FsLayout,
    inodes: &[InodeDisk],
) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let max_bytes = (layout.inode_table_blocks as usize) * block_size;

    // Serializamos todos los inodos
    let mut data = slice_of_structs_to_bytes(inodes);

    if data.len() > max_bytes {
        // Opción 1: truncar silenciosamente (no recomendado en prod)
        // data.truncate(max_bytes);

        // Opción 2: fallar explícitamente:
        return Err(anyhow!(
            "La tabla de inodos ({:?} bytes) no cabe en los bloques reservados ({:?} bytes)",
            data.len(),
            max_bytes
        ));
    }

    // Rellenar con ceros hasta ocupar exactamente la región
    if data.len() < max_bytes {
        data.resize(max_bytes, 0);
    }

    write_blocks(entries, layout.inode_table_start, &data)
}

/// Escribe el bitmap de bloques libres.
fn write_bitmap(
    entries: &[PathBuf],
    layout: &FsLayout,
    bitmap: &[u8],
) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let max_bytes = (layout.free_bitmap_blocks as usize) * block_size;

    if bitmap.len() > max_bytes {
        return Err(anyhow!(
            "El bitmap ({:?} bytes) no cabe en los bloques reservados ({:?} bytes)",
            bitmap.len(),
            max_bytes
        ));
    }

    let mut data = bitmap.to_vec();
    if data.len() < max_bytes {
        data.resize(max_bytes, 0);
    }

    write_blocks(entries, layout.free_bitmap_start, &data)
}


/// Rellena los bloques de datos con ceros.
fn zero_data_blocks(entries: &[PathBuf], layout: &FsLayout) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let zero_block = vec![0u8; block_size];

    let start = layout.data_blocks_start as usize;
    let end = layout.total_blocks as usize;

    for i in start..end {
        write_block(&entries[i], &zero_block)?;
    }

    Ok(())
}

/// Serializa una estructura arbitraria (repr(C), Copy) a bytes.
fn struct_to_bytes<T: Copy>(val: &T) -> Vec<u8> {
    let size = mem::size_of::<T>();
    unsafe {
        let ptr = val as *const T as *const u8;
        std::slice::from_raw_parts(ptr, size).to_vec()
    }
}

/// Serializa un slice de estructuras (repr(C), Copy) a bytes contiguos.
fn slice_of_structs_to_bytes<T: Copy>(slice: &[T]) -> Vec<u8> {
    let size = mem::size_of::<T>() * slice.len();
    unsafe {
        let ptr = slice.as_ptr() as *const u8;
        std::slice::from_raw_parts(ptr, size).to_vec()
    }
}

/// Escribe un bloque lógico completo sobre el archivo correspondiente.
fn write_block(path: &PathBuf, data: &[u8]) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let mut buf = vec![0u8; block_size];

    let to_copy = data.len().min(block_size);
    buf[..to_copy].copy_from_slice(&data[..to_copy]);

    let mut file = File::create(path)
        .with_context(|| format!("No se pudo crear/escribir el archivo {:?}", path))?;
    file.write_all(&buf)?;
    Ok(())
}

/// Escribe datos en varios bloques consecutivos, comenzando en `start_block`.
fn write_blocks(
    entries: &[PathBuf],
    start_block: u32,
    data: &[u8],
) -> Result<()> {
    let block_size = QRFS_BLOCK_SIZE as usize;
    let mut offset = 0usize;
    let mut block_index = start_block as usize;

    while offset < data.len() {
        if block_index >= entries.len() {
            return Err(anyhow!(
                "No hay suficientes bloques para escribir los metadatos (se quedó corto en el bloque {}).",
                block_index
            ));
        }

        let end = (offset + block_size).min(data.len());
        write_block(&entries[block_index], &data[offset..end])?;
        offset = end;
        block_index += 1;
    }

    Ok(())
}
