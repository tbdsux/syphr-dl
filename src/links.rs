//! Turning text into download links: lists, wildcard patterns, and the links on a web page.

use std::collections::{HashSet, VecDeque};

use reqwest::Url;

/// Most links one pattern or page grab may produce.
pub const MAX_LINKS: usize = 10_000;
/// Most pages one grab fetches, the first page included.
const MAX_PAGES: usize = 30;
/// Pages bigger than this aren't read for links.
const MAX_PAGE_BYTES: u64 = 5 << 20;

/// File types the clipboard watcher and the grabber look for, until the user changes them.
pub const DEFAULT_TYPES: &str = "3gp 7z aac apk appimage avi bz2 deb dmg epub exe flac gz iso m4a m4b m4v mkv mov mp3 \
     mp4 mpeg mpg msi ogg opus pdf pkg rar rpm tar tgz wav webm wma wmv xz zip zst";

pub fn is_web_link(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() > "https://".len()
}

/// The web links in `text`, one or more per line, in order and without repeats; lines starting with `#` are comments.
pub fn parse_list(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .flat_map(str::split_whitespace)
        .filter(|word| is_web_link(word) && seen.insert(*word))
        .map(String::from)
        .collect()
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

/// Expands every `[1-50]`, `[001-120]` or `[a-z]` in `pattern` into one link per value; leading zeros pad.
pub fn expand_pattern(pattern: &str) -> Result<Vec<String>, String> {
    let mut out = vec![String::new()];
    let mut ranges = 0;
    let mut rest = pattern.trim();
    while let Some(open) = rest.find('[') {
        let (before, after) = rest.split_at(open);
        out.iter_mut().for_each(|link| link.push_str(before));
        match after.find(']').and_then(|close| Some((close, range(&after[1..close])?))) {
            Some((close, values)) => {
                let values = values?;
                if out.len().saturating_mul(values.len()) > MAX_LINKS {
                    return Err(format!("That makes more than {MAX_LINKS} links. Use a smaller range."));
                }
                out = out.iter().flat_map(|link| values.iter().map(move |v| format!("{link}{v}"))).collect();
                ranges += 1;
                rest = &after[close + 1..];
            }
            // Not a range, such as an IPv6 host: keep the bracket as typed.
            None => {
                out.iter_mut().for_each(|link| link.push('['));
                rest = &after[1..];
            }
        }
    }
    out.iter_mut().for_each(|link| link.push_str(rest));
    if ranges == 0 {
        return Err("Add a range such as [1-50], [001-120] or [a-z] where the address changes.".into());
    }
    Ok(out)
}

/// The values of one `start-end` range, either way round, or None when it isn't one.
fn range(inner: &str) -> Option<Result<Vec<String>, String>> {
    let (a, b) = inner.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        let (x, y): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
        if x.abs_diff(y) >= MAX_LINKS as u64 {
            return Some(Err(format!("A range can make at most {MAX_LINKS} links.")));
        }
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 0 };
        let values: Vec<u64> = if x <= y { (x..=y).collect() } else { (y..=x).rev().collect() };
        return Some(Ok(values.into_iter().map(|n| format!("{n:0width$}")).collect()));
    }
    let letter = |s: &str| (s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic()).then(|| s.as_bytes()[0]);
    let (x, y) = (letter(a)?, letter(b)?);
    if x.is_ascii_lowercase() != y.is_ascii_lowercase() {
        return None;
    }
    let values: Vec<u8> = if x <= y { (x..=y).collect() } else { (y..=x).rev().collect() };
    Some(Ok(values.into_iter().map(|c| char::from(c).to_string()).collect()))
}

/// Every `href` and `src` in `html` that resolves to a web link, in page order, without fragments or repeats.
pub fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    // shortcut: a plain attribute scan; ignores <base href>, srcset and links built by scripts. Use an HTML parser if
    // pages that need those matter.
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut from = 0;
    while let Some((start, len)) =
        ["href", "src"].iter().filter_map(|attr| Some((from + lower[from..].find(attr)?, attr.len()))).min()
    {
        from = start + len;
        // Attribute names follow whitespace; this skips words like `data-href` and text such as `ahref`.
        if !lower[..start].ends_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(rest) = html[from..].trim_start().strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next(),
            _ => rest.split(|c: char| c.is_ascii_whitespace() || c == '>').next(),
        };
        let Some(value) = value.map(|v| v.trim().replace("&amp;", "&")).filter(|v| !v.is_empty()) else { continue };
        let Ok(mut url) = base.join(&value) else { continue };
        url.set_fragment(None);
        if matches!(url.scheme(), "http" | "https") && seen.insert(url.clone()) {
            found.push(url);
        }
    }
    found
}

/// Whether a link probably leads to another page rather than a file, so the grabber may follow it.
fn is_page(url: &Url) -> bool {
    extension(url.as_str()).is_none_or(|ext| matches!(ext.as_str(), "htm" | "html" | "php" | "asp" | "aspx" | "jsp"))
}

/// The links on `page`, and with `depth` > 0 on the pages of the same site it links to, that many levels down.
/// Needs the engine's tokio runtime. Only the first page failing is an error.
pub async fn grab(page: &str, depth: usize) -> Result<Vec<String>, String> {
    let client = crate::engine::client()?;
    let start = Url::parse(page.trim()).map_err(|_| "Enter a page address that starts with http:// or https://")?;
    let mut queue = VecDeque::from([(start.clone(), 0)]);
    let mut visited = HashSet::from([start.clone()]);
    let (mut links, mut seen) = (Vec::new(), HashSet::new());
    let mut fetched = 0;
    while let Some((url, level)) = queue.pop_front() {
        if fetched == MAX_PAGES {
            break;
        }
        fetched += 1;
        let html = match fetch_page(&client, &url).await {
            Ok(html) => html,
            Err(e) if fetched == 1 => return Err(e),
            Err(_) => continue,
        };
        for link in extract_links(&html, &url) {
            if level < depth && link.host() == start.host() && is_page(&link) && visited.insert(link.clone()) {
                queue.push_back((link.clone(), level + 1));
            }
            if links.len() < MAX_LINKS && seen.insert(link.clone()) {
                links.push(link.to_string());
            }
        }
    }
    Ok(links)
}

async fn fetch_page(client: &reqwest::Client, url: &Url) -> Result<String, String> {
    let response = client.get(url.clone()).send().await.map_err(|e| format!("Couldn't open the page: {e}"))?;
    let response = response.error_for_status().map_err(|e| format!("The page couldn't be loaded: {e}"))?;
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|t| t.to_str().ok())
        .is_none_or(|t| t.contains("html"));
    if !html || response.content_length().is_some_and(|len| len > MAX_PAGE_BYTES) {
        return Err("That address isn't a web page.".into());
    }
    response.text().await.map_err(|e| format!("Couldn't read the page: {e}"))
}

/// The links in a dropped or imported file: a text list, or a `.webloc` / `.url` web shortcut. Large files are
/// skipped rather than read, in case a download itself was dropped.
pub fn links_in_file(path: &std::path::Path) -> Vec<String> {
    if std::fs::metadata(path).map_or(true, |m| !m.is_file() || m.len() > MAX_PAGE_BYTES) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("webloc" | "url") => shortcut_link(&text).into_iter().collect(),
        _ => parse_list(&text),
    }
}

/// The link inside a `.webloc` (macOS) or `.url` (Windows) web shortcut.
pub fn shortcut_link(text: &str) -> Option<String> {
    let link = match text.find("<string>") {
        Some(start) => text[start + "<string>".len()..].split("</string>").next()?,
        None => text.lines().find_map(|line| line.trim().strip_prefix("URL="))?,
    };
    let link = link.trim().replace("&amp;", "&");
    is_web_link(&link).then_some(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_patterns() {
        assert_eq!(
            expand_pattern("https://x/img[1-3].jpg").unwrap(),
            ["https://x/img1.jpg", "https://x/img2.jpg", "https://x/img3.jpg"]
        );
        let padded = expand_pattern("https://x/img[001-120].jpg").unwrap();
        assert_eq!(
            (padded.len(), padded[0].as_str(), padded[119].as_str()),
            (120, "https://x/img001.jpg", "https://x/img120.jpg")
        );
        assert_eq!(expand_pattern("https://x/[a-c]").unwrap(), ["https://x/a", "https://x/b", "https://x/c"]);
        assert_eq!(expand_pattern("https://x/[C-A]").unwrap(), ["https://x/C", "https://x/B", "https://x/A"]);
        assert_eq!(
            expand_pattern("https://x/[1-2]/[a-b]").unwrap(),
            ["https://x/1/a", "https://x/1/b", "https://x/2/a", "https://x/2/b"]
        );
        // Brackets that aren't ranges stay as typed.
        assert_eq!(expand_pattern("http://[::1]/f[8-9]").unwrap(), ["http://[::1]/f8", "http://[::1]/f9"]);
        assert!(expand_pattern("https://x/[a-Z]").is_err());
        assert!(expand_pattern("https://x/file.zip").is_err());
        assert!(expand_pattern("https://x/[1-20000]").is_err());
        assert!(expand_pattern("https://x/[1-200]/[1-200]").is_err());
        assert!(expand_pattern("https://x/[1-99999999999999999999999]").is_err());
    }

    #[test]
    fn parses_link_lists() {
        let text = "# my links\nhttps://a.com/1.zip\n\n  https://a.com/2.zip  not-a-link https://a.com/3.zip\n\
                    #https://a.com/skipped.zip\nftp://a.com/x\nhttps://a.com/1.zip\n";
        assert_eq!(parse_list(text), ["https://a.com/1.zip", "https://a.com/2.zip", "https://a.com/3.zip"]);
        assert!(parse_list("").is_empty());
    }

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

    #[test]
    fn extracts_links() {
        let base = Url::parse("https://site.com/dir/page.html").unwrap();
        let html = r##"<a href="file.zip">x</a> <A HREF='/abs.pdf#p2'> <img src=pic.jpg>
            <a data-href="nope"> <a href="mailto:me@x.com"> <a href = "https://other.com/v.mp4?a=1&amp;b=2">
            <a href="file.zip"> <script src="//cdn.com/s.js"></script> <a href="#top">"##;
        let links: Vec<String> = extract_links(html, &base).into_iter().map(String::from).collect();
        assert_eq!(
            links,
            [
                "https://site.com/dir/file.zip",
                "https://site.com/abs.pdf",
                "https://site.com/dir/pic.jpg",
                "https://other.com/v.mp4?a=1&b=2",
                "https://cdn.com/s.js",
                "https://site.com/dir/page.html",
            ]
        );
    }

    #[test]
    fn reads_dropped_files() {
        let dir = std::env::temp_dir().join(format!("syphr-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let list = file("links.txt", "# saved\nhttps://a.com/1.zip\nhttps://a.com/2.zip\n");
        assert_eq!(links_in_file(&list), ["https://a.com/1.zip", "https://a.com/2.zip"]);
        let webloc = file("x.WEBLOC", "<plist><dict><string>https://a.com/w</string></dict></plist>");
        assert_eq!(links_in_file(&webloc), ["https://a.com/w"]);
        assert!(links_in_file(&file("bin.dat", "\u{0}\u{1}")).is_empty());
        assert!(links_in_file(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn grabs_links_from_same_site_pages() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut lines = BufReader::new(&stream).lines().map_while(Result::ok);
                let path = lines.next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
                lines.take_while(|line| !line.is_empty()).for_each(drop);
                let body = match path.as_str() {
                    "/" => r#"<a href="/a.zip"> <a href="more.html"> <a href="https://other.example/x.html">"#,
                    "/more.html" => r#"<a href="/b.pdf"> <a href="/deeper.html">"#,
                    "/deeper.html" => "<a href=/c.zip>",
                    _ => "",
                };
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length";
                write!(stream, "{head}: {}\r\n\r\n{body}", body.len()).ok();
            }
        });
        let grab_at = |depth| crate::engine::RUNTIME.block_on(grab(&format!("{base}/"), depth)).unwrap();
        let has = |links: &[String], path: &str| links.contains(&format!("{base}{path}"));
        let top = grab_at(0);
        assert!(has(&top, "/a.zip") && has(&top, "/more.html") && !has(&top, "/b.pdf"));
        let one = grab_at(1);
        assert!(has(&one, "/b.pdf") && has(&one, "/deeper.html") && !has(&one, "/c.zip"));
        assert!(has(&grab_at(2), "/c.zip"));
        assert!(crate::engine::RUNTIME.block_on(grab("not a link", 1)).is_err());
    }

    #[test]
    fn reads_web_shortcuts() {
        let webloc = "<plist><dict><key>URL</key><string>https://a.com/x?a=1&amp;b=2</string></dict></plist>";
        assert_eq!(shortcut_link(webloc).as_deref(), Some("https://a.com/x?a=1&b=2"));
        assert_eq!(shortcut_link("[InternetShortcut]\r\nURL=https://a.com/y\r\n").as_deref(), Some("https://a.com/y"));
        assert_eq!(shortcut_link("hello"), None);
    }
}
