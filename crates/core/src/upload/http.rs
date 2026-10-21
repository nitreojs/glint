use std::path::PathBuf;

pub enum Part {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<Part>,
}

#[derive(Default)]
pub struct Response {
    pub status: u32,
    pub text: String,
    pub headers: Vec<(String, String)>,
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, PartialEq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn split_url(url: &str) -> Result<Url, String> {
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

pub fn parse_headers(raw: &str) -> Vec<(String, String)> {
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
