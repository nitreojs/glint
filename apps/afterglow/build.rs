use std::path::PathBuf;

use glint_core::icon::{EXE_SIZES, ico};

/// afterglow's mark: glint's, in amber.
const AMBER: [u8; 3] = [0xF5, 0x9E, 0x0B];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("afterglow.ico");
    std::fs::write(&icon, ico(&EXE_SIZES, AMBER)).expect("write afterglow.ico");
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
            VALUE "FileDescription", "afterglow"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "afterglow"
            VALUE "LegalCopyright", "Copyright (C) starkow"
            VALUE "OriginalFilename", "afterglow.exe"
            VALUE "ProductName", "afterglow"
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
    let rc_path = out.join("afterglow.rc");
    std::fs::write(&rc_path, rc).expect("write afterglow.rc");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().expect("compile afterglow.rc");
}
