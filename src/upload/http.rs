use std::ffi::c_void;
use std::io::Read;
use std::path::PathBuf;

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR};

pub(crate) enum Part {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub(crate) struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<Part>,
}

#[derive(Default)]
pub(crate) struct Response {
    pub status: u32,
    pub text: String,
    pub headers: Vec<(String, String)>,
    pub url: String,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub(crate) fn split_url(url: &str) -> Result<Url, String> {
    let bad = || format!("not a web address: {url}");
    let (secure, rest) = match url.split_once("://") {
        Some((s, rest)) if s.eq_ignore_ascii_case("https") => (true, rest),
        Some((s, rest)) if s.eq_ignore_ascii_case("http") => (false, rest),
        _ => return Err(bad()),
    };
    let rest = rest.split('#').next().unwrap_or_default();
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(cut);
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (h, p.parse().map_err(|_| bad())?),
        _ => (authority, if secure { 443 } else { 80 }),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(bad());
    }
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    Ok(Url { secure, host: host.to_string(), port, path })
}

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Handle, String> {
        if h.is_null() { Err(failure(what, windows::core::Error::from_thread())) } else { Ok(Handle(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn failure(what: &str, e: windows::core::Error) -> String {
    let code = (e.code().0 as u32) & 0xFFFF;
    let reason = match code {
        ERROR_WINHTTP_TIMEOUT => "the server took too long to answer".to_string(),
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "the server's name couldn't be resolved (offline?)".to_string(),
        ERROR_WINHTTP_CANNOT_CONNECT => "couldn't connect to the server".to_string(),
        ERROR_WINHTTP_CONNECTION_ERROR => "the connection was reset".to_string(),
        ERROR_WINHTTP_SECURE_FAILURE => "the secure connection failed".to_string(),
        _ if e.message().is_empty() => format!("error {code}"),
        _ => e.message(),
    };
    format!("{what}: {reason}")
}

const CHUNK: usize = 64 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

pub(crate) fn send(req: &Request) -> Result<Response, String> {
    let url = split_url(&req.url)?;
    let mut total = 0u64;
    for part in &req.body {
        total += match part {
            Part::Bytes(b) => b.len() as u64,
            Part::File(p) => std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.len(),
        };
    }
    let total = u32::try_from(total).map_err(|_| "files over 4 GB can't be uploaded".to_string())?;
    let mut headers = String::new();
    for (k, v) in &req.headers {
        headers += &format!("{k}: {v}\r\n");
    }
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                &HSTRING::from(concat!("glint/", env!("CARGO_PKG_VERSION"))),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, 0, 30_000, 60_000, 120_000).map_err(|e| failure("WinHttpSetTimeouts", e))?;
        let decompress = WINHTTP_DECOMPRESSION_FLAG_GZIP | WINHTTP_DECOMPRESSION_FLAG_DEFLATE;
        let _ = WinHttpSetOption(Some(session.0), WINHTTP_OPTION_DECOMPRESSION, Some(&decompress.to_ne_bytes()));
        let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(&url.host), url.port, 0), &url.host)?;
        let flags = if url.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = Handle::new(
            WinHttpOpenRequest(
                connect.0,
                &HSTRING::from(req.method),
                &HSTRING::from(&url.path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "WinHttpOpenRequest",
        )?;
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(request.0, (!headers.is_empty()).then_some(&headers[..]), None, 0, total, 0)
            .map_err(|e| failure(&url.host, e))?;
        for part in &req.body {
            match part {
                Part::Bytes(b) => write_all(&request, b)?,
                Part::File(p) => {
                    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                    let mut buf = vec![0u8; CHUNK];
                    loop {
                        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", p.display()))?;
                        if n == 0 {
                            break;
                        }
                        write_all(&request, &buf[..n])?;
                    }
                }
            }
        }
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| failure(&url.host, e))?;
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&raw mut status as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| failure("status", e))?;
        let raw = query_string(|buf, len| {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_RAW_HEADERS_CRLF,
                PCWSTR::null(),
                buf,
                len,
                std::ptr::null_mut(),
            )
        });
        let final_url = query_string(|buf, len| WinHttpQueryOption(request.0, WINHTTP_OPTION_URL, buf, len));
        let mut body = Vec::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), CHUNK as u32, &mut read)
                .map_err(|e| failure(&url.host, e))?;
            if read == 0 || body.len() >= MAX_RESPONSE {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
        }
        Ok(Response {
            status,
            text: String::from_utf8_lossy(&body).into_owned(),
            headers: parse_headers(&raw),
            url: if final_url.is_empty() { req.url.clone() } else { final_url },
        })
    }
}

unsafe fn write_all(request: &Handle, mut data: &[u8]) -> Result<(), String> {
    while !data.is_empty() {
        let mut written = 0u32;
        let n = data.len().min(CHUNK) as u32;
        unsafe { WinHttpWriteData(request.0, Some(data.as_ptr().cast()), n, &mut written) }
            .map_err(|e| failure("upload", e))?;
        data = &data[written as usize..];
    }
    Ok(())
}

fn query_string(query: impl Fn(Option<*mut c_void>, *mut u32) -> windows::core::Result<()>) -> String {
    let mut bytes = 0u32;
    let _ = query(None, &mut bytes);
    if bytes == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; bytes as usize / 2 + 1];
    if query(Some(buf.as_mut_ptr().cast()), &mut bytes).is_err() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..bytes as usize / 2])
}

fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_host_port_and_path() {
        let u = split_url("https://uguu.se/upload.php?output=text#x").unwrap();
        assert_eq!(u, Url { secure: true, host: "uguu.se".into(), port: 443, path: "/upload.php?output=text".into() });
        let u = split_url("HTTP://user@example.com:8080?a=1").unwrap();
        assert_eq!(u, Url { secure: false, host: "example.com".into(), port: 8080, path: "/?a=1".into() });
        assert_eq!(split_url("http://[::1]:81/x").unwrap().host, "::1");
        assert!(split_url("ftp://x").is_err() && split_url("https://").is_err() && split_url("example.com").is_err());
    }

    #[test]
    fn raw_headers_parse() {
        let h = parse_headers("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nLocation: https://a/b:c\r\n\r\n");
        let r = Response { headers: h, ..Default::default() };
        assert_eq!(r.header("location"), Some("https://a/b:c"));
        assert_eq!(r.header("content-type"), Some("text/plain"));
    }
}
