// ROMs inside .zip archives (mGBA's VFS zip support + "Load ROM in
// archive..."). The chosen entry is extracted to a cache file so the normal
// path-based loader can open it.

use std::io::Read;
use std::path::{Path, PathBuf};

const ROM_EXTS: [&str; 7] = ["gba", "agb", "bin", "gb", "gbc", "sgb", "cgb"];

pub fn is_archive(path: &Path) -> bool {
    path.extension().map_or(false, |e| e.eq_ignore_ascii_case("zip"))
}

fn open(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    zip::ZipArchive::new(f).map_err(|e| format!("{}: {e}", path.display()))
}

/// ROM-looking entries in the archive.
pub fn list_roms(path: &Path) -> Result<Vec<String>, String> {
    let z = open(path)?;
    Ok(z.file_names()
        .filter(|n| {
            Path::new(n)
                .extension()
                .map_or(false, |e| ROM_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .map(str::to_string)
        .collect())
}

/// Extract `entry` to `<cache>/rgba/archive/<name>` and return that path.
pub fn extract_to_cache(path: &Path, entry: &str) -> Result<PathBuf, String> {
    let mut z = open(path)?;
    let mut f = z.by_name(entry).map_err(|e| format!("{entry}: {e}"))?;
    let mut data = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut data).map_err(|e| e.to_string())?;
    let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("rgba").join("archive");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = Path::new(entry).file_name().ok_or("bad entry name")?;
    let out = dir.join(name);
    std::fs::write(&out, data).map_err(|e| e.to_string())?;
    Ok(out)
}
