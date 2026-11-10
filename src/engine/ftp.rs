//! `ftp://` links: the size from `SIZE`, and each part read from its own connection starting at `REST <offset>`.
//! Plain FTP only; FTPS isn't supported.

use std::{pin::Pin, time::Duration};

use reqwest::Url;
use suppaftp::tokio::AsyncFtpStream;
use suppaftp::types::FileType;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Access, LinkInfo, err, names, proxy};

const TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn is_ftp(url: &str) -> bool {
    url.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("ftp://"))
}

/// A logged-in control connection, and the file's path on the server.
async fn connect(url: &str, access: &Access) -> Result<(AsyncFtpStream, String), String> {
    // FTP can't go through the proxy, and must not quietly go around it.
    if proxy()?.is_some() {
        return Err("FTP links can't go through the proxy. Turn the proxy off in Settings to download them.".into());
    }
    let parsed = Url::parse(url).map_err(err)?;
    let host = parsed.host_str().ok_or("The FTP link has no server.")?;
    let address = format!("{host}:{}", parsed.port().unwrap_or(21));
    let path = names::percent_decode(parsed.path());
    let mut ftp = timed(AsyncFtpStream::connect(address)).await?;
    let (user, password) = access.login(url).unwrap_or_else(|| ("anonymous".into(), "anonymous@".into()));
    timed(ftp.login(user, password)).await?;
    timed(ftp.transfer_type(FileType::Binary)).await?;
    Ok((ftp, path))
}

async fn timed<T, E: std::error::Error>(work: impl Future<Output = Result<T, E>>) -> Result<T, String> {
    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| "The FTP server stopped responding.".to_string())?
        .map_err(err)
}

/// Servers that answer `SIZE` can resume and split with `REST`; the rest get one connection from the start.
pub(super) async fn inspect(url: &str, access: &Access) -> Result<LinkInfo, String> {
    let (mut ftp, path) = connect(url, access).await?;
    let total = timed(ftp.size(&path)).await.ok().map(|size| size as u64);
    let _ = ftp.quit().await;
    let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("download").to_string();
    let summary = format!(
        "→ {} · {}",
        total.map(crate::download::format_bytes).unwrap_or_else(|| "size unknown".into()),
        if total.is_some() { "resume supported" } else { "no resume" },
    );
    Ok(LinkInfo { name, total, ranged: total.is_some(), content_type: None, summary })
}

/// The file's bytes from `offset`; the control connection stays open while they're read.
pub(super) struct Body {
    _control: AsyncFtpStream,
    data: Pin<Box<dyn AsyncRead + Send>>,
}

impl Body {
    pub(super) async fn open(url: &str, access: &Access, offset: u64) -> Result<Self, String> {
        let (mut ftp, path) = connect(url, access).await?;
        if offset > 0 {
            timed(ftp.resume_transfer(offset as usize)).await?;
        }
        let data = timed(ftp.retr_as_stream(&path)).await?;
        Ok(Self { _control: ftp, data: Box::pin(data) })
    }

    /// Fills `buf` with the next bytes; false at the end of the file.
    pub(super) async fn read(&mut self, buf: &mut Vec<u8>) -> Result<bool, String> {
        buf.resize(64 * 1024, 0);
        let n = tokio::time::timeout(TIMEOUT, self.data.read(buf))
            .await
            .map_err(|_| "The FTP server stopped sending.".to_string())?
            .map_err(err)?;
        buf.truncate(n);
        Ok(n > 0)
    }
}
