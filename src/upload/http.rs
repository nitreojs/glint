use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, PWSTR};

use super::proxy::{self, Route, Setting};
use crate::platform::log::log;

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
    if e.message().is_empty() { format!("{what}: error {code}") } else { format!("{what}: {}", e.message()) }
}

fn network_failure(stage: &str, host: &str, route: &Route, e: windows::core::Error) -> String {
    let code = (e.code().0 as u32) & 0xFFFF;
    let proxy_up = match route {
        Route::Proxy(p) if code == ERROR_WINHTTP_CANNOT_CONNECT => proxy::listening(p),
        _ => false,
    };
    log(&format!("winhttp {stage} to {host} {route}: error {code}{}", if proxy_up { ", proxy is up" } else { "" }));
    reason(code, host, route, proxy_up).unwrap_or_else(|| failure(host, e))
}

fn reason(code: u32, host: &str, route: &Route, proxy_up: bool) -> Option<String> {
    let via = match route {
        Route::Direct => String::new(),
        Route::Proxy(p) => format!(" (through the proxy {p})"),
    };
    Some(match (code, route) {
        (ERROR_WINHTTP_CANNOT_CONNECT, Route::Proxy(p)) if proxy_up => format!(
            "the proxy {p} couldn't reach {host}: the host is down or the proxy's route to it failed; try again or another host"
        ),
        (ERROR_WINHTTP_CANNOT_CONNECT, Route::Proxy(p)) => {
            format!("couldn't connect to the proxy {p} to reach {host}: is it running? (settings → uploads → proxy)")
        }
        (ERROR_WINHTTP_CANNOT_CONNECT, Route::Direct) => format!(
            "couldn't reach {host}: it's down or blocked on this network; set a proxy in settings → uploads, or pick another host"
        ),
        (ERROR_WINHTTP_NAME_NOT_RESOLVED, Route::Proxy(p)) => format!("the proxy {p} couldn't be found"),
        (ERROR_WINHTTP_NAME_NOT_RESOLVED, Route::Direct) => {
            format!("{host} couldn't be found (offline, or dns blocked?)")
        }
        (ERROR_WINHTTP_TIMEOUT, _) => format!("{host} took too long to answer{via}"),
        (ERROR_WINHTTP_CONNECTION_ERROR, _) => format!("the connection to {host} was cut off{via}"),
        (ERROR_WINHTTP_INVALID_SERVER_RESPONSE, _) => format!("{host} sent an answer glint couldn't read{via}"),
        (
            ERROR_WINHTTP_SECURE_FAILURE
            | ERROR_WINHTTP_SECURE_CHANNEL_ERROR
            | ERROR_WINHTTP_SECURE_FAILURE_PROXY
            | ERROR_WINHTTP_SECURE_CERT_CN_INVALID
            | ERROR_WINHTTP_SECURE_CERT_DATE_INVALID
            | ERROR_WINHTTP_SECURE_CERT_REVOKED
            | ERROR_WINHTTP_SECURE_CERT_REV_FAILED
            | ERROR_WINHTTP_SECURE_CERT_WRONG_USAGE
            | ERROR_WINHTTP_SECURE_INVALID_CA
            | ERROR_WINHTTP_SECURE_INVALID_CERT,
            _,
        ) => format!("the secure connection to {host} failed{via}"),
        _ => return None,
    })
}

const CHUNK: usize = 64 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

pub(crate) fn send(req: &Request, proxy: &Setting, progress: &mut dyn FnMut(u64, u64)) -> Result<Response, String> {
    let mut body = Vec::new();
    let mut response = exchange(req, proxy, progress, &mut |data| {
        body.extend_from_slice(data);
        Ok(body.len() < MAX_RESPONSE)
    })?;
    response.text = String::from_utf8_lossy(&body).into_owned();
    Ok(response)
}

/// Streams the response body into `path`, refusing more than `max` bytes; the returned
/// response has no text. The file is removed unless the server answered 2xx.
pub(crate) fn download(req: &Request, proxy: &Setting, path: &Path, max: u64) -> Result<Response, String> {
    let shown = path.display();
    let mut file = std::fs::File::create(path).map_err(|e| format!("{shown}: {e}"))?;
    let mut written = 0u64;
    let result = exchange(req, proxy, &mut |_, _| {}, &mut |data| {
        written += data.len() as u64;
        if written > max {
            return Err(format!("{} is bigger than the {max} bytes expected", req.url));
        }
        file.write_all(data).map_err(|e| format!("{shown}: {e}"))?;
        Ok(true)
    });
    let flushed = file.sync_all().map_err(|e| format!("{shown}: {e}"));
    drop(file);
    let result = result.and_then(|r| flushed.map(|_| r));
    if !result.as_ref().is_ok_and(Response::ok) {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Sends `req`; `sink` gets the response body in chunks and returns false to stop reading.
fn exchange(
    req: &Request,
    proxy: &Setting,
    progress: &mut dyn FnMut(u64, u64),
    sink: &mut dyn FnMut(&[u8]) -> Result<bool, String>,
) -> Result<Response, String> {
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
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, 0, 30_000, 60_000, 120_000).map_err(|e| failure("WinHttpSetTimeouts", e))?;
        let decompress = WINHTTP_DECOMPRESSION_FLAG_GZIP | WINHTTP_DECOMPRESSION_FLAG_DEFLATE;
        let _ = WinHttpSetOption(Some(session.0), WINHTTP_OPTION_DECOMPRESSION, Some(&decompress.to_ne_bytes()));
        let route = proxy::route(proxy, session.0, &req.url, &url.host, url.secure)?;
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
        if let Route::Proxy(p) = &route {
            let mut name: Vec<u16> = p.encode_utf16().chain([0]).collect();
            let info = WINHTTP_PROXY_INFO {
                dwAccessType: WINHTTP_ACCESS_TYPE_NAMED_PROXY,
                lpszProxy: PWSTR(name.as_mut_ptr()),
                lpszProxyBypass: PWSTR::null(),
            };
            let bytes = std::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<WINHTTP_PROXY_INFO>());
            WinHttpSetOption(Some(request.0), WINHTTP_OPTION_PROXY, Some(bytes))
                .map_err(|e| failure(&format!("proxy {p}"), e))?;
        }
        let net = |stage: &str, e| network_failure(stage, &url.host, &route, e);
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(request.0, (!headers.is_empty()).then_some(&headers[..]), None, 0, total, 0)
            .map_err(|e| net("send", e))?;
        let mut sent = 0u64;
        let mut write = |data: &[u8]| -> Result<(), String> {
            write_all(&request, data).map_err(|e| net("write", e))?;
            sent += data.len() as u64;
            progress(sent, u64::from(total));
            Ok(())
        };
        for part in &req.body {
            match part {
                Part::Bytes(b) => write(b)?,
                Part::File(p) => {
                    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                    let mut buf = vec![0u8; CHUNK];
                    loop {
                        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", p.display()))?;
                        if n == 0 {
                            break;
                        }
                        write(&buf[..n])?;
                    }
                }
            }
        }
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| net("receive", e))?;
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
        let mut buf = vec![0u8; CHUNK];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), CHUNK as u32, &mut read).map_err(|e| net("read", e))?;
            if read == 0 || !sink(&buf[..read as usize])? {
                break;
            }
        }
        Ok(Response {
            status,
            text: String::new(),
            headers: parse_headers(&raw),
            url: if final_url.is_empty() { req.url.clone() } else { final_url },
        })
    }
}

unsafe fn write_all(request: &Handle, mut data: &[u8]) -> windows::core::Result<()> {
    while !data.is_empty() {
        let mut written = 0u32;
        let n = data.len().min(CHUNK) as u32;
        unsafe { WinHttpWriteData(request.0, Some(data.as_ptr().cast()), n, &mut written) }?;
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

    #[test]
    fn network_errors_name_the_host_and_route() {
        let proxy = Route::Proxy("127.0.0.1:10809".into());
        let r = |code, route: &Route| reason(code, "catbox.moe", route, false).unwrap();
        assert!(r(ERROR_WINHTTP_CANNOT_CONNECT, &Route::Direct).starts_with("couldn't reach catbox.moe: "));
        assert!(r(ERROR_WINHTTP_CANNOT_CONNECT, &Route::Direct).contains("set a proxy in settings"));
        assert!(r(ERROR_WINHTTP_CANNOT_CONNECT, &proxy).starts_with("couldn't connect to the proxy 127.0.0.1:10809"));
        assert!(r(ERROR_WINHTTP_CANNOT_CONNECT, &proxy).contains("catbox.moe"));
        let up = reason(ERROR_WINHTTP_CANNOT_CONNECT, "catbox.moe", &proxy, true).unwrap();
        assert!(up.starts_with("the proxy 127.0.0.1:10809 couldn't reach catbox.moe: "));
        assert_eq!(r(ERROR_WINHTTP_NAME_NOT_RESOLVED, &proxy), "the proxy 127.0.0.1:10809 couldn't be found");
        assert_eq!(r(ERROR_WINHTTP_TIMEOUT, &Route::Direct), "catbox.moe took too long to answer");
        assert_eq!(
            r(ERROR_WINHTTP_CONNECTION_ERROR, &proxy),
            "the connection to catbox.moe was cut off (through the proxy 127.0.0.1:10809)"
        );
        assert!(r(ERROR_WINHTTP_SECURE_INVALID_CA, &Route::Direct).starts_with("the secure connection to catbox.moe"));
        assert_eq!(reason(ERROR_WINHTTP_RESEND_REQUEST, "catbox.moe", &Route::Direct, false), None);
    }
}
