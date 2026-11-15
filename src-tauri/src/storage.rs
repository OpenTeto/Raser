use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub fn get_storage_path() -> PathBuf {
    if let Ok(app_data) = std::env::var("APPDATA") {
        let p = PathBuf::from(app_data).join("Raser").join("storage.json");
        return p;
    }
    PathBuf::from("storage.json")
}

pub fn read_storage_raw() -> String {
    let path = get_storage_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
                return content;
            }
        }
    }
    "{}".to_string()
}

pub fn read_storage_map() -> HashMap<String, serde_json::Value> {
    let raw = read_storage_raw();
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_storage_map(data: HashMap<String, serde_json::Value>) {
    let path = get_storage_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json_str) = serde_json::to_string_pretty(&data) {
        let _ = fs::write(&path, json_str);
    }
}

pub fn get_cache_dir() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("Raser").join("cache")
    } else if let Ok(app_data) = std::env::var("APPDATA") {
        PathBuf::from(app_data).join("Raser").join("cache")
    } else {
        PathBuf::from("cache")
    }
}

pub fn get_audio_cache_dir() -> PathBuf {
    let dir = get_cache_dir().join("audio");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn get_lyrics_cache_dir() -> PathBuf {
    let dir = get_cache_dir().join("lyrics");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn sanitize_cache_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

pub fn get_cached_audio_path(track_id: &str) -> Option<PathBuf> {
    if track_id.is_empty() { return None; }
    let clean_id = sanitize_cache_key(track_id);
    let p = get_audio_cache_dir().join(format!("{}.bin", clean_id));
    if p.exists() && p.metadata().map(|m| m.len() > 1024).unwrap_or(false) {
        Some(p)
    } else {
        None
    }
}

pub fn save_cached_audio(track_id: &str, bytes: &[u8]) {
    if track_id.is_empty() || bytes.is_empty() { return; }
    let clean_id = sanitize_cache_key(track_id);
    let p = get_audio_cache_dir().join(format!("{}.bin", clean_id));
    let _ = fs::write(p, bytes);
}

pub fn get_cached_lyrics(track_id: &str) -> Option<String> {
    if track_id.is_empty() { return None; }
    let clean_id = sanitize_cache_key(track_id);
    let p = get_lyrics_cache_dir().join(format!("{}.json", clean_id));
    if p.exists() {
        fs::read_to_string(p).ok()
    } else {
        None
    }
}

pub fn save_cached_lyrics(track_id: &str, json_content: &str) {
    if track_id.is_empty() || json_content.is_empty() { return; }
    let clean_id = sanitize_cache_key(track_id);
    let p = get_lyrics_cache_dir().join(format!("{}.json", clean_id));
    let _ = fs::write(p, json_content);
}

pub fn get_cache_stats() -> (u64, usize) {
    let mut total_bytes = 0u64;
    let mut file_count = 0usize;

    for dir in &[get_audio_cache_dir(), get_lyrics_cache_dir()] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        total_bytes += meta.len();
                        file_count += 1;
                    }
                }
            }
        }
    }
    (total_bytes, file_count)
}

pub fn clear_cache() {
    let _ = fs::remove_dir_all(get_cache_dir());
    let _ = fs::create_dir_all(get_audio_cache_dir());
    let _ = fs::create_dir_all(get_lyrics_cache_dir());
}
