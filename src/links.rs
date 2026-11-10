//! Turning text into download links.

/// File types the clipboard watcher looks for, until the user changes them.
pub const DEFAULT_TYPES: &str = "3gp 7z aac apk appimage avi bz2 deb dmg epub exe flac gz iso m4a m4b m4v mkv mov mp3 \
     mp4 mpeg mpg msi ogg opus pdf pkg rar rpm tar tgz wav webm wma wmv xz zip zst";

pub fn is_web_link(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() > "https://".len()
}

/// The link a copied `text` holds, if it is one link to a video site or to a file of one of `types`.
pub fn offered_link(text: &str, types: &str) -> Option<String> {
    let link = text.trim();
    let one_link = is_web_link(link) && !link.contains(char::is_whitespace);
    (one_link && (crate::engine::ytdlp::handles(link) || has_type(link, types))).then(|| link.to_string())
}

/// The lowercase extension of a link's file name, if it has one.
pub fn extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    // A bare host such as `https://example.com` has no file name.
    if path.matches('/').count() < 3 {
        return None;
    }
    name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).filter(|ext| !ext.is_empty())
}

/// Whether the link's extension is one of `types` (separated by spaces or commas, any case, dots allowed).
pub fn has_type(url: &str, types: &str) -> bool {
    let Some(ext) = extension(url) else { return false };
    types.split([' ', ',']).map(|t| t.trim().trim_start_matches('.')).any(|t| t.eq_ignore_ascii_case(&ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_types() {
        assert!(has_type("https://a.com/x/Setup.EXE?v=1", "zip exe"));
        assert!(has_type("https://a.com/a.tar.gz", ".gz, zip"));
        assert!(!has_type("https://a.com/page.html", DEFAULT_TYPES));
        assert!(!has_type("https://a.com", "com"));
        assert!(!has_type("https://a.com/download", DEFAULT_TYPES));
    }

    #[test]
    fn offers_copied_file_and_video_links() {
        assert_eq!(offered_link(" https://a.com/x.zip\n", "zip").as_deref(), Some("https://a.com/x.zip"));
        assert!(offered_link("https://youtu.be/abc", "zip").is_some());
        assert!(offered_link("https://a.com/page.html", DEFAULT_TYPES).is_none());
        assert!(offered_link("https://a.com/x.zip https://a.com/y.zip", "zip").is_none());
        assert!(offered_link("see https://a.com/x.zip", "zip").is_none());
        assert!(offered_link("https://a.com/x.zip", "").is_none());
    }
}
