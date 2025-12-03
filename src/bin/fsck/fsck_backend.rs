/*Define la interfaz para el backend del fsck.
Define un trait que describe cómo el fsck debe leer:
bloques, inodos, el directorio raíz
Existe para permitir múltiples backends, por ejemplo:
Un mock (lo que usa ahorita), el FS real de compañeros (cuando esté listo), pruebas de fragmentación
*/

use super::fsck_types::*;

pub trait FsckBackend {
    fn load_superblock(&self) -> Superblock;
    fn load_all_inodes(&self) -> Vec<Inode>;
    fn read_inode(&self, ino: u32) -> Option<Inode>;
    fn read_block(&self, block: u32) -> Option<Vec<u8>>;
    fn read_dir(&self, ino: u32) -> Vec<Dirent>;
    fn load_block_bitmap(&self) -> Vec<bool>;
}
