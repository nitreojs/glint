//! afterglow's update end to end against a release list served from this machine: the check,
//! the download with its progress, the verification, the staged copy and the swap, all in a
//! temporary folder.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use glint_core::update::AFTERGLOW;
use glint_core::update::version::Version;
use glint_core::upload::proxy::Setting;

use super::apply::{self, Step};
use super::verify::tests::{AFTERGLOW_EXE, AFTERGLOW_SIG, AFTERGLOW_SUMS, AFTERGLOW_TEST_KEY};
use super::{Found, Job, Source, run, staged_in};

const EXE_NAME: &str = "afterglow-v0.2.0-windows-x64.exe";

/// Serves the files `files` makes for the port (path → body) over plain HTTP on a free local
/// port, one request per connection; the returned list collects the paths asked for.
fn serve(files: impl FnOnce(u16) -> Vec<(String, Vec<u8>)>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let files = files(port);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let path = line.split(' ').nth(1).unwrap_or("").to_string();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
            }
            log.lock().unwrap().push(path.clone());
            let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", b"not here".to_vec()),
            };
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&body));
        }
    });
    (port, asked)
}

/// A release list with glint's newest release above afterglow's, and afterglow 0.2.0 whose exe
/// is `exe`.
fn release_list(port: u16, exe: &[u8]) -> Vec<(String, Vec<u8>)> {
    let url = |name: &str| format!("http://127.0.0.1:{port}/dl/{name}");
    let asset = |name: &str, size: usize| format!(r#"{{"name":"{name}","browser_download_url":"{}","size":{size}}}"#, url(name));
    let sig = format!("{EXE_NAME}.minisig");
    let list = format!(
        r#"[{{"tag_name":"v9.0.0","draft":false,"prerelease":false,"assets":[{},{},{}]}},
            {{"tag_name":"afterglow-v0.2.0","draft":false,"prerelease":false,"assets":[{},{},{}]}}]"#,
        asset("glint-v9.0.0-windows-x64.exe", 10),
        asset("glint-v9.0.0-windows-x64.exe.minisig", 10),
        asset("SHA256SUMS", 10),
        asset(EXE_NAME, exe.len()),
        asset(&sig, AFTERGLOW_SIG.len()),
        asset("SHA256SUMS", AFTERGLOW_SUMS.len()),
    );
    vec![
        ("/releases".into(), list.into_bytes()),
        (format!("/dl/{EXE_NAME}"), exe.to_vec()),
        (format!("/dl/{sig}"), AFTERGLOW_SIG.as_bytes().to_vec()),
        ("/dl/SHA256SUMS".into(), AFTERGLOW_SUMS.as_bytes().to_vec()),
    ]
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glint-update-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn job(download: bool) -> Job {
    Job { beta: false, download, manual: true, install: false, candidate: None, proxy: Setting::None }
}

fn source(port: u16) -> Source {
    Source { api: format!("http://127.0.0.1:{port}/releases"), key: AFTERGLOW_TEST_KEY.into() }
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn afterglow_finds_downloads_verifies_and_swaps_in_its_update() {
    let (port, asked) = serve(|port| release_list(port, AFTERGLOW_EXE));
    let root = temp("afterglow");
    let downloads = root.join("update");
    let current = v("0.1.0");
    let src = source(port);
    let mut seen = Vec::new();

    // notify only: the check names afterglow's release, not glint's newer one
    let found = run(&AFTERGLOW, &job(false), &src, &current, &downloads, &mut |_, p| seen.push(p)).unwrap();
    let Found::Available(c) = found else { panic!("afterglow 0.2.0 should be available") };
    assert_eq!((c.tag.as_str(), c.exe.name.as_str()), ("afterglow-v0.2.0", EXE_NAME));
    assert!(seen.is_empty() && !downloads.exists(), "a check alone downloads nothing");

    // auto: downloaded, verified and kept, with the progress climbing to 100
    let found = run(&AFTERGLOW, &job(true), &src, &current, &downloads, &mut |_, p| seen.push(p)).unwrap();
    let Found::Ready(staged) = found else { panic!("the update should be ready") };
    assert_eq!((staged.tag.as_str(), staged.version.to_string().as_str()), ("afterglow-v0.2.0", "0.2.0"));
    assert_eq!(staged.exe, downloads.join(EXE_NAME));
    assert_eq!(std::fs::read(&staged.exe).unwrap(), AFTERGLOW_EXE);
    assert_eq!((seen.first(), seen.last()), (Some(&0), Some(&100)));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    let paths = asked.lock().unwrap().clone();
    assert!(paths.iter().all(|p| !p.contains("glint-v9")), "glint's files are never fetched: {paths:?}");

    // what's staged verifies again at install time; glint doesn't take afterglow's file
    assert_eq!(staged_in(&AFTERGLOW, &downloads, AFTERGLOW_TEST_KEY, &current), Some(staged.clone()));
    assert_eq!(staged_in(&glint_core::update::GLINT, &downloads, AFTERGLOW_TEST_KEY, &current), None);

    // the swap: the running exe moves aside, the update takes its place and is started
    let install = root.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let exe = install.join("afterglow.exe");
    std::fs::write(&exe, b"the running afterglow 0.1.0").unwrap();
    let aside = install.join("afterglow.exe.old");
    let args = vec!["--updated".to_string(), "0.1.0".to_string()];
    let mut launched = Vec::new();
    let steps = apply::plan(&exe, &aside, &staged.exe, &args);
    apply::run(&steps, &mut |step| match step {
        Step::Move { from, to } => apply::move_file(from, to),
        Step::Launch { exe, args } => {
            launched.push((exe.clone(), args.clone()));
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), AFTERGLOW_EXE);
    assert_eq!(std::fs::read(&aside).unwrap(), b"the running afterglow 0.1.0");
    assert_eq!(launched, [(exe.clone(), args)]);
    assert!(!staged.exe.exists());

    // once running 0.2.0 there's nothing newer
    let found = run(&AFTERGLOW, &job(true), &src, &v("0.2.0"), &downloads, &mut |_, _| {}).unwrap();
    assert!(matches!(found, Found::UpToDate));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_damaged_download_is_refused_and_not_kept() {
    let mut damaged = AFTERGLOW_EXE.to_vec();
    damaged[30] ^= 1;
    let (port, _) = serve(|port| release_list(port, &damaged));
    let downloads = temp("damaged").join("update");
    let err = run(&AFTERGLOW, &job(true), &source(port), &v("0.1.0"), &downloads, &mut |_, _| {})
        .err()
        .expect("a damaged exe is refused");
    assert!(err.starts_with(&format!("{EXE_NAME} is damaged")), "{err}");
    let left: Vec<_> = std::fs::read_dir(&downloads).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.iter().all(|n| !n.to_string_lossy().ends_with(".exe") && !n.to_string_lossy().ends_with(".part")), "{left:?}");
    let _ = std::fs::remove_dir_all(downloads.parent().unwrap_or(&downloads));
}
