use colored::*;
use qrfs::fsck::{mock::MockBackend, fsck_types::*, fsck};

fn main() {
    // ——————————————————————————————————————————
    // BACKEND SIMULADO (con errores para mostrar colores)
    // ——————————————————————————————————————————
    let backend = MockBackend {
    superblock: Superblock {
        magic: 0x1234,
        num_inodes: 2,
        num_blocks: 10,
        root_inode: 0,
    },

    inodes: vec![
        Inode {
            is_dir: true,
            size: 0,
            direct: vec![],
            indirect1: None,
            indirect2: None,
        },
        Inode {
            is_dir: false,
            size: 5,
            direct: vec![1],
            indirect1: None,
            indirect2: None,
        }
    ],

    dirs: vec![
        vec![
            Dirent { name: ".".into(),  inode: 0, is_dir: true, valid: true },
            Dirent { name: "..".into(), inode: 0, is_dir: true, valid: true },
            Dirent { name: "file".into(), inode: 1, is_dir: false, valid: true },
        ]
    ],

    blocks: vec![ vec![]; 10 ],

    bitmap: vec![
        false,  // 0 libre
        true,   // 1 usado por inode 1
        false, false, false,
        false, false, false, false, false,
    ],
};


    // ——————————————————————————————————————————
    //       EJECUTAR FSCK
    // ——————————————————————————————————————————
    let rep = fsck::run_fsck(&backend);

    println!("\n{}", " QRFS FILESYSTEM CHECK ".on_blue().bold());
    println!("{}", "──────────────────────────────────────────".blue());

    // ——————————————————————————————————————————
    //       RESULTADOS DE BLOQUES
    // ——————————————————————————————————————————
    println!("\n{}", "Bloques".bold().underline());

    if rep.blocks_ok {
        println!("  {} Bloques OK", "✓".green());
    } else {
        println!("  {} Errores en bloques", "✗".red());
    }

    // ——————————————————————————————————————————
    //       RESULTADOS DE INODOS
    // ——————————————————————————————————————————
    println!("\n{}", "Inodos".bold().underline());

    if rep.inodes_ok {
        println!("  {} Inodos OK", "✓".green());
    } else {
        println!("  {} Errores en inodos", "✗".red());
    }

    // ——————————————————————————————————————————
    //       ERRORES DETALLADOS
    // ——————————————————————————————————————————
    println!("\n{}", "Errores detectados".bold().underline());

    if rep.errors.is_empty() {
        println!("  {} No se encontraron errores", "✓".green());
    } else {
        for err in &rep.errors {
            println!("  {} {}", "•".red(), err.red());
        }
    }

    // ——————————————————————————————————————————
    //       RESUMEN FINAL
    // ——————————————————————————————————————————
    println!("\n{}", "Resumen".bold().underline());

    if rep.errors.is_empty() {
        println!("{} Sistema de archivos limpio.\n", "✓ OK".green().bold());
    } else {
        println!(
            "{} {} errores encontrados.\n",
            "✗ FSCK completado con errores:".red().bold(),
            rep.errors.len().to_string().yellow()
        );
    }
}
