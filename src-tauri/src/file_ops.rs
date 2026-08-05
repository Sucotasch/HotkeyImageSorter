use std::path::Path;
use crate::models::ImageInfo;
use jwalk::WalkDir;
use std::time::SystemTime;

pub fn list_images(dir_path: &str) -> Vec<ImageInfo> {
    let mut images = Vec::new();
    let extensions = ["jpg", "jpeg", "png", "webp", "jif"];

    for entry in WalkDir::new(dir_path)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext.to_lowercase().as_str()) {
                    let metadata = entry.metadata().ok();
                    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    images.push(ImageInfo {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path: path.to_string_lossy().to_string(),
                        size,
                        extension: ext.to_string(),
                        modified,
                    });
                }
            }
        }
    }
    images
}

pub fn move_image(source: &str, target_dir: &str) -> Result<String, String> {
    let source_path = Path::new(source);
    let target_dir_path = Path::new(target_dir);

    if !target_dir_path.exists() {
        std::fs::create_dir_all(target_dir_path).map_err(|e| e.to_string())?;
    }

    let file_name = source_path.file_name().ok_or("Invalid filename")?;
    let mut target_path = target_dir_path.join(file_name);

    // Auto-rename if exists
    let mut count = 1;
    let stem = source_path.file_stem().ok_or("Invalid file stem")?.to_string_lossy();
    let ext = source_path.extension().ok_or("Invalid extension")?.to_string_lossy();

    while target_path.exists() {
        let new_name = format!("{}_{}.{}", stem, count, ext);
        target_path = target_dir_path.join(new_name);
        count += 1;
    }

    std::fs::rename(source_path, &target_path).map_err(|e| e.to_string())?;
    
    Ok(target_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn move_image_moves_and_returns_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let src = a.join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let moved = move_image(src.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(moved, b.join("pic.jpg").to_string_lossy().to_string());
        assert!(!src.exists());
        assert!(b.join("pic.jpg").exists());
    }

    #[test]
    fn move_image_renames_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("pic.jpg"), b"data1").unwrap();

        let moved = move_image(a.join("pic.jpg").to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&moved), b.join("pic.jpg"));

        // Second file with the same name must be auto-renamed, not overwritten
        fs::write(a.join("pic.jpg"), b"data2").unwrap();
        let moved2 = move_image(a.join("pic.jpg").to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&moved2), b.join("pic_1.jpg"));
        assert_eq!(fs::read_to_string(b.join("pic.jpg")).unwrap(), "data1");
        assert_eq!(fs::read_to_string(b.join("pic_1.jpg")).unwrap(), "data2");
    }

    #[test]
    fn move_image_creates_missing_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b/deep/nested");
        fs::create_dir_all(&a).unwrap();
        let src = a.join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let moved = move_image(src.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert!(Path::new(&moved).exists());
        assert!(!src.exists());
    }

    #[test]
    fn move_image_fails_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path().join("b");
        fs::create_dir_all(&b).unwrap();
        let missing = dir.path().join("nope.jpg");
        assert!(move_image(missing.to_str().unwrap(), b.to_str().unwrap()).is_err());
    }
}
