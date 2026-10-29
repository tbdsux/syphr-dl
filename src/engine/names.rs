//! Picking and cleaning the names files are saved under.

use std::path::{Path, PathBuf};

use reqwest::{Response, header};

/// Content-Disposition first, then the last path segment after redirects, then a guess from the content type.
pub(super) fn response_file_name(resp: &Response) -> String {
    let disposition = resp.headers().get(header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok());
    let content_type = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
    pick_file_name(disposition, resp.url().path(), content_type)
}

fn pick_file_name(disposition: Option<&str>, url_path: &str, content_type: Option<&str>) -> String {
    let from_header = disposition.and_then(disposition_file_name);
    let from_url = url_path.rsplit('/').find(|s| !s.is_empty()).map(percent_decode);
    let name = sanitize(&from_header.or(from_url).unwrap_or_default());
    let name = if name.is_empty() { "download".to_string() } else { name };
    if Path::new(&name).extension().is_some() {
        return name;
    }
    match content_type.and_then(extension_for) {
        Some(ext) => format!("{name}.{ext}"),
        None => name,
    }
}

/// The typed name wins; without an extension it keeps the server's.
pub(super) fn chosen_name(typed: Option<&str>, server: String) -> String {
    let Some(typed) = typed.map(sanitize).filter(|n| !n.is_empty()) else { return server };
    match (Path::new(&typed).extension(), Path::new(&server).extension()) {
        (None, Some(ext)) => format!("{typed}.{}", ext.to_string_lossy()),
        _ => typed,
    }
}

fn disposition_file_name(value: &str) -> Option<String> {
    let params: Vec<(String, &str)> = value
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim()))
        .collect();
    // RFC 5987: filename*=UTF-8''na%20me.zip wins over the plain one.
    if let Some((_, v)) = params.iter().find(|(k, _)| k == "filename*") {
        let encoded = v.splitn(3, '\'').nth(2).unwrap_or(v);
        return Some(percent_decode(encoded.trim_matches('"')));
    }
    params.iter().find(|(k, _)| k == "filename").map(|(_, v)| v.trim_matches('"').to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex =
            (bytes[i] == b'%').then(|| s.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Keeps a server-supplied name from escaping the folder or hiding the file.
pub(super) fn sanitize(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') || c.is_control() { '_' } else { c }).collect();
    cleaned.trim().trim_start_matches('.').trim().to_string()
}

fn extension_for(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" | "application/x-rar-compressed" => "rar",
        "application/x-apple-diskimage" => "dmg",
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        _ => return None,
    })
}

pub(super) fn file_name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub(super) fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// "name.zip", then "name (1).zip", … skipping names whose file or partial file exists.
pub(super) fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string());
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (0..)
        .map(|n| if n == 0 { folder.join(name) } else { folder.join(format!("{stem} ({n}){ext}")) })
        .find(|p| !p.exists() && !part_path(p).exists())
        .expect("an unbounded search finds a free name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    #[test]
    fn names_from_headers_url_and_type() {
        assert_eq!(pick_file_name(Some("attachment; filename=\"report.pdf\""), "/dl", None), "report.pdf");
        assert_eq!(
            pick_file_name(Some("attachment; filename=\"a.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"), "/dl", None),
            "résumé.pdf"
        );
        assert_eq!(pick_file_name(None, "/files/My%20Song.mp3", None), "My Song.mp3");
        assert_eq!(pick_file_name(None, "/download", Some("application/zip; charset=binary")), "download.zip");
        assert_eq!(pick_file_name(None, "/", None), "download");
        assert_eq!(pick_file_name(Some("attachment; filename=\"../../.bashrc\""), "/", None), "_.._.bashrc");
        assert_eq!(chosen_name(Some("notes"), "report.pdf".into()), "notes.pdf");
        assert_eq!(chosen_name(Some("notes.txt"), "report.pdf".into()), "notes.txt");
        assert_eq!(chosen_name(Some("  "), "report.pdf".into()), "report.pdf");
        assert_eq!(chosen_name(Some("a/b"), "x".into()), "a_b");
    }

    #[test]
    fn unique_names_skip_files_and_partials() {
        let dir = std::env::temp_dir().join(format!("syphr-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        File::create(dir.join("a.zip")).unwrap();
        File::create(dir.join("a (1).zip.part")).unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a (2).zip"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
