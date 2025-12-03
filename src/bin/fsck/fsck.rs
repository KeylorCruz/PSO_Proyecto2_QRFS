/*EL ARCHIVO PRINCIPAL DE FSCK. Aquí esta la función principal, 
validaciones básicas como leer superblock, validar inodos, validar bloques,
recopilar errores. Ahora mismo es simple */
use super::{fsck_backend::FsckBackend, fsck_types::*};

fn check_superblock<B: FsckBackend>(
    backend: &B,
    report: &mut FsckReport
) {
    let sb = backend.load_superblock();
    let inodes = backend.load_all_inodes();
    let bitmap = backend.load_block_bitmap();

    // 1. Magic number
    if sb.magic != 0x1234 {
        report.errors.push("Superblock: magic inválido".into());
    }

    // 2. Coincidencia del número de inodos
    if sb.num_inodes as usize != inodes.len() {
        report.errors.push(format!(
            "Superblock: num_inodes = {}, pero hay {} inodos reales",
            sb.num_inodes,
            inodes.len()
        ));
        report.inodes_ok = false;
    }

    // 3. Coincidencia del número de bloques
    if sb.num_blocks as usize != bitmap.len() {
        report.errors.push(format!(
            "Superblock: num_blocks = {}, pero bitmap tiene {} entradas",
            sb.num_blocks,
            bitmap.len()
        ));
        report.blocks_ok = false;
    }

    // 4. root_inode válido
    if sb.root_inode as usize >= inodes.len() {
        report.errors.push(format!(
            "Superblock: root_inode ({}) fuera de rango",
            sb.root_inode
        ));
        report.inodes_ok = false;
    }

    // 5. Reglas básicas que nunca deben violarse
    if sb.num_blocks == 0 {
        report.errors.push("Superblock: num_blocks no puede ser 0".into());
        report.blocks_ok = false;
    }

    if sb.num_inodes == 0 {
        report.errors.push("Superblock: num_inodes no puede ser 0".into());
        report.inodes_ok = false;
    }
}

fn check_bitmap_global<B: FsckBackend>(backend: &B, sb: &Superblock, report: &mut FsckReport) {
    let bitmap = backend.load_block_bitmap();
    let inodes = backend.load_all_inodes();

    // 1. Tamaño incorrecto
    if bitmap.len() != sb.num_blocks as usize {
        report.errors.push(format!(
            "Bitmap tiene tamaño incorrecto: {} en vez de {}",
            bitmap.len(),
            sb.num_blocks
        ));
        report.blocks_ok = false;
        return;
    }

    // 2. Bloques realmente usados por inodos
    let mut used_by_inodes = vec![false; sb.num_blocks as usize];

    for inode in &inodes {
        for &blk in &inode.direct {
            if blk < sb.num_blocks {
                used_by_inodes[blk as usize] = true;
            }
        }
        if let Some(blk) = inode.indirect1 {
            if blk < sb.num_blocks {
                used_by_inodes[blk as usize] = true;
            }
        }
        if let Some(blk) = inode.indirect2 {
            if blk < sb.num_blocks {
                used_by_inodes[blk as usize] = true;
            }
        }
    }

    // 3. Comparación bitmap <-> realidad
    for block in 0..sb.num_blocks as usize {
        let bitmap_says_used = bitmap[block];
        let inode_says_used = used_by_inodes[block];

        if bitmap_says_used && !inode_says_used {
            report.errors.push(format!(
                "Bitmap marca usado el bloque {}, pero ningún inodo lo usa",
                block
            ));
            report.blocks_ok = false;
        }

        if !bitmap_says_used && inode_says_used {
            report.errors.push(format!(
                "Bitmap marca libre el bloque {}, pero algún inodo lo usa",
                block
            ));
            report.blocks_ok = false;
        }
    }
}


fn check_dirs<B: FsckBackend>(backend: &B, report: &mut FsckReport) {
    let sb = backend.load_superblock();
    let inodes = backend.load_all_inodes();

    // Si el root inode es inválido, no tiene sentido seguir
    if report.errors.iter().any(|e| e.contains("Superblock: root_inode")) {
        return;
    }

    // Validar que root sea directorio
    if sb.root_inode as usize >= inodes.len() {
        report.errors.push("Root inode fuera de rango".into());
        report.inodes_ok = false;
        return;
    }
    if !inodes[sb.root_inode as usize].is_dir {
        report.errors.push("Root inode no es un directorio".into());
        report.inodes_ok = false;
    }

    // Validar cada directorio
    for (ino_id, inode) in inodes.iter().enumerate() {
        if inode.is_dir {
            let entries = backend.read_dir(ino_id as u32);

            for entry in entries {
                // Nombre vacío
                if entry.name.is_empty() {
                    report.errors.push(format!(
                        "Inodo {}: dirent con nombre vacío",
                        ino_id
                    ));
                }

                // Inodo fuera de rango
                if entry.inode as usize >= inodes.len() {
                    report.errors.push(format!(
                        "Inodo {}: dirent '{}' apunta a inodo inexistente ({})",
                        ino_id,
                        entry.name,
                        entry.inode
                    ));
                    continue; // ← ¡Evita que leamos un índice inválido!
                }

                // Tipo no concuerda
                let target = &inodes[entry.inode as usize];
                    if entry.is_dir != target.is_dir {
                        report.errors.push(format!(
                            "Dirent '{}' en inodo {} declara tipo incorrecto",
                            entry.name,
                            ino_id
                        ));
                    }
            }
        }
    }
}

fn check_orphan_inodes<B: FsckBackend>(backend: &B, report: &mut FsckReport) {
    let sb = backend.load_superblock();
    let inodes = backend.load_all_inodes();

    // Mapa: inodos referenciados por algún directorio
    let mut referenced = vec![false; inodes.len()];

    // El root SIEMPRE se considera referenciado
    if sb.root_inode < inodes.len() as u32 {
        referenced[sb.root_inode as usize] = true;
    }

    // Recorrer los directorios para marcar referencias
    for (ino_id, inode) in inodes.iter().enumerate() {
        if inode.is_dir {
            for entry in backend.read_dir(ino_id as u32) {
                if entry.inode < inodes.len() as u32 {
                    referenced[entry.inode as usize] = true;
                }
            }
        }
    }

    // Finalmente: detectar huérfanos
    for ino in 0..inodes.len() {
        if !referenced[ino] {
            report.errors.push(format!("Inodo {} huérfano", ino));
            report.inodes_ok = false;
        }
    }
}


fn check_blocks_global<B: FsckBackend>(backend: &B, _sb: &Superblock, report: &mut FsckReport) {
    let mut seen = std::collections::HashSet::new();

    for (ino_id, inode) in backend.load_all_inodes().iter().enumerate() {

        // Recorrer directos
        for &blk in &inode.direct {
            if !seen.insert(blk) {
                report.errors.push(format!(
                    "Inodo {}: bloque duplicado globalmente ({})",
                    ino_id, blk
                ));
                report.blocks_ok = false;
            }
        }

        // indirecto 1
        if let Some(blk) = inode.indirect1 {
            if !seen.insert(blk) {
                report.errors.push(format!(
                    "Inodo {}: indirect1 duplicado globalmente ({})",
                    ino_id, blk
                ));
                report.blocks_ok = false;
            }
        }

        // indirecto 2
        if let Some(blk) = inode.indirect2 {
            if !seen.insert(blk) {
                report.errors.push(format!(
                    "Inodo {}: indirect2 duplicado globalmente ({})",
                    ino_id, blk
                ));
                report.blocks_ok = false;
            }
        }
    }
}



fn check_inodes_basic<B: FsckBackend>(backend: &B, report: &mut FsckReport) {
    let sb = backend.load_superblock();
    let total_blocks = sb.num_blocks;

    // Recorremos todos los inodos que el backend expone
    for (idx, inode) in backend.load_all_inodes().iter().enumerate() {
        
        // 1. Valida tamaño
        if inode.size == u32::MAX {
            report.errors.push(format!("Inodo {} tiene tamaño inválido", idx));
            report.inodes_ok = false;
        }

        // 2. Valida punteros directos
        for &blk in &inode.direct {
            if blk >= total_blocks {
                report.errors.push(format!(
                    "Inodo {}: bloque directo fuera de rango ({})",
                    idx, blk
                ));
                report.inodes_ok = false;
            }
        }

        // 3. Valida puntero indirecto 1
        if let Some(blk) = inode.indirect1 {
            if blk >= total_blocks {
                report.errors.push(format!(
                    "Inodo {}: indirect1 fuera de rango ({})",
                    idx, blk
                ));
                report.inodes_ok = false;
            }
        }

        // 4. Valida puntero indirecto 2
        if let Some(blk) = inode.indirect2 {
            if blk >= total_blocks {
                report.errors.push(format!(
                    "Inodo {}: indirect2 fuera de rango ({})",
                    idx, blk
                ));
                report.inodes_ok = false;
            }
        }

        // 5. Detectar duplicados dentro del mismo inodo
        let mut seen = std::collections::HashSet::new();
        for &blk in &inode.direct {
            if !seen.insert(blk) {
                report.errors.push(format!(
                    "Inodo {}: bloque duplicado ({})",
                    idx, blk
                ));
                report.inodes_ok = false;
            }
        }
    }
}



pub fn run_fsck<B: FsckBackend>(backend: &B) -> FsckReport {
    let mut report = FsckReport::new();

    // --- Paso 1: Validación del superblock ---
    check_superblock(backend, &mut report);

    // --- Paso 2: Validación básica de inodos ---
    check_inodes_basic(backend, &mut report);

    // --- Paso 3: Validación global de bloques ---
    let sb = backend.load_superblock();
    check_blocks_global(backend, &sb, &mut report);

    // --- Paso 4: Validación de directorios ---
    check_dirs(backend, &mut report);

    // --- Paso 5: Validación del bitmap global ---
    check_bitmap_global(backend, &sb, &mut report);

    // --- Paso 6: Detección de inodos huérfanos ---
    check_orphan_inodes(backend, &mut report);

    println!("(TEMPORAL) fsck ejecutado. Superblock magic = {}", sb.magic);
    report
}

