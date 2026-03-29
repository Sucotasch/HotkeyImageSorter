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
