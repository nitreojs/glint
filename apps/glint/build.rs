use std::path::PathBuf;

use glint_core::config::{DEFAULT_ACCENT, parse_hex};
use glint_core::icon::{EXE_SIZES, ico};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("glint.ico");
    let accent = parse_hex(DEFAULT_ACCENT).expect("the default accent is a colour");
    std::fs::write(&icon, ico(&EXE_SIZES, accent)).expect("write glint.ico");
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let numbers: Vec<&str> = version.split(['-', '+']).next().unwrap_or_default().split('.').collect();
    let binary = format!("{},0", numbers.join(","));
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {binary}
PRODUCTVERSION {binary}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "starkow"
            VALUE "FileDescription", "glint"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "glint"
            VALUE "LegalCopyright", "Copyright (C) starkow"
            VALUE "OriginalFilename", "glint.exe"
            VALUE "ProductName", "glint"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("glint.rc");
    std::fs::write(&rc_path, rc).expect("write glint.rc");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().expect("compile glint.rc");
}
