//! Finding glint, whose trimmer opens afterglow's clips when it's there: the exe its "open in
//! glint editor" Explorer entry starts, else a glint that's running.

use std::path::{Path, PathBuf};

use glint_core::wide::from_wstr;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// The command of glint's Explorer entry on videos.
const VERB: &str = r"Software\Classes\SystemFileAssociations\.mp4\shell\glint.edit\command";
const EXE: &str = "glint.exe";
/// What makes glint open a video in its trimmer.
pub const CLIP_ARG: &str = "--clip";

/// glint's exe, if it's installed or running.
pub fn find() -> Option<PathBuf> {
    command(VERB).and_then(|c| exe_of(&c)).filter(|p| p.is_file()).or_else(|| running(EXE))
}

/// Opens `video` in glint's trimmer.
pub fn trim(glint: &Path, video: &Path) -> Result<(), String> {
    std::process::Command::new(glint)
        .arg(CLIP_ARG)
        .arg(video)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't open glint: {e}"))
}

/// The default value of `key` under HKEY_CURRENT_USER, a string.
fn command(key: &str) -> Option<String> {
    let key = HSTRING::from(key);
    let mut bytes = 0u32;
    unsafe {
        let asked = RegGetValueW(HKEY_CURRENT_USER, &key, PCWSTR::null(), RRF_RT_REG_SZ, None, None, Some(&mut bytes));
        if asked != ERROR_SUCCESS || bytes < 2 {
            return None;
        }
        let mut buf = vec![0u16; bytes.div_ceil(2) as usize];
        let read = RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
        (read == ERROR_SUCCESS).then(|| from_wstr(&buf))
    }
}

/// The program a command line starts: quoted (`"C:\glint\glint.exe" --clip "%1"`) or up to the
/// first space.
fn exe_of(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split_whitespace().next()?,
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// Where a running process named `name` (`glint.exe`) was started from.
fn running(name: &str) -> Option<PathBuf> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more && found.is_none() {
            if from_wstr(&entry.szExeFile).eq_ignore_ascii_case(name) {
                found = image(entry.th32ProcessID);
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// The exe process `pid` runs.
fn image(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{REG_SZ, RegDeleteTreeW, RegSetKeyValueW};

    #[test]
    fn the_exe_comes_out_of_a_command_line() {
        let exe = |c: &str| exe_of(c).map(|p| p.display().to_string());
        assert_eq!(exe(r#""C:\Program Files\glint\glint.exe" --clip "%1""#).as_deref(), Some(r"C:\Program Files\glint\glint.exe"));
        assert_eq!(exe(r"F:\glint\glint.exe --clip %1").as_deref(), Some(r"F:\glint\glint.exe"));
        assert_eq!(exe("  "), None);
        assert_eq!(exe(r#""" --clip"#), None);
    }

    #[test]
    fn the_explorer_entry_names_glint() {
        // a key of its own, so the real entry is never touched
        let key = format!(r"Software\glint-test-{}\command", std::process::id());
        let value = r#""F:\glint\glint.exe" --clip "%1""#;
        let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
        let set = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str()), PCWSTR::null(), REG_SZ.0, Some(data.as_ptr().cast()), (data.len() * 2) as u32)
        };
        assert_eq!(set, ERROR_SUCCESS);
        let read = command(&key);
        let parent = key.trim_end_matches(r"\command");
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(parent));
        }
        assert_eq!(read.as_deref(), Some(value));
        assert_eq!(read.and_then(|c| exe_of(&c)), Some(PathBuf::from(r"F:\glint\glint.exe")));
        assert_eq!(command(&key), None);
    }

    #[test]
    fn a_running_process_is_found_by_its_name() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().into_owned();
        let found = running(&name).expect("this test's own process");
        assert!(found.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case(&name), "{}", found.display());
        assert_eq!(running("no-such-program-here.exe"), None);
    }
}
