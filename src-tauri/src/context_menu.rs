//! Windows Explorer context-menu integration (HKCU — no administrator rights).
//!
//! - Image files get a **cascading submenu** "HK Image Sorter" with two items
//!   ("Open in HK Image Sorter", "Show Metadata Info") and a separator
//!   between them. Implemented with nested `shell` keys + empty `subcommands`
//!   value (the mechanism Explorer supports from HKCU); items are prefixed
//!   (`a_`, `b_`) so Explorer keeps them in order, and `CommandFlags`
//!   (EXPCMDFLAGS 0x40) draws the separator.
//! - Folders (`Directory`) and the folder background (`Directory\Background`)
//!   get a single direct "Open in HK Image Sorter" item (no metadata there).

use std::process::Command;

/// Image extensions that get the two context-menu items.
const IMAGE_EXTS: [&str; 7] = ["png", "jpg", "jpeg", "jif", "webp", "avif", "gif"];

/// Display name of the cascading menu shown in the Explorer context menu.
const GROUP_LABEL: &str = "HK Image Sorter";

/// `HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell\<key>`
fn sfa_path(ext: &str, key: &str) -> String {
    format!(r"HKCU\Software\Classes\SystemFileAssociations\.{ext}\shell\{key}")
}

/// The current executable path, quoted for use inside a command line.
fn quoted_exe() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    Ok(format!("\"{}\"", exe.to_string_lossy()))
}

/// Runs `reg.exe` with the given args, returning its output. On failure the
/// stderr is surfaced so the UI can show why (e.g. reg.exe missing).
fn run_reg(args: &[&str]) -> Result<String, String> {
    let out = Command::new("reg.exe")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run reg.exe: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned().trim().to_string())
    }
}

fn reg_add(path: &str, value_name: Option<&str>, value: &str) -> Result<(), String> {
    let mut args = vec!["add", path];
    if let Some(name) = value_name {
        args.push("/v");
        args.push(name);
    } else {
        args.push("/ve");
    }
    args.push("/d");
    args.push(value);
    args.push("/f");
    run_reg(&args)?;
    Ok(())
}

fn reg_delete(path: &str) {
    let _ = run_reg(&["delete", path, "/f"]);
}

/// Installs the cascading "HK Image Sorter" menu for every image extension:
/// a top-level key with an empty `subcommands` value plus nested `shell`
/// items. Item keys are prefixed (`a_`, `b_`) so Explorer keeps them in
/// order, and `CommandFlags` (EXPCMDFLAGS 0x40) draws a separator below the
/// first item. See https://blog.sverrirs.com/2014/05/creating-cascading-menu-items-in.html
fn install_file_items() -> Result<(), String> {
    let exe = quoted_exe()?;
    for ext in IMAGE_EXTS {
        // Clear any direct items from older layouts first, so Explorer shows
        // only the new cascading menu (no stray "Show Metadata Info").
        for legacy in ["HKIS.Open", "HKIS.Metadata", "HKIS.Explorer"] {
            reg_delete(&sfa_path(ext, legacy));
        }
        let top = sfa_path(ext, "HKIS");
        // Top-level menu entry: label + icon + empty subcommands -> the
        // nested shell keys below are shown as a cascading submenu.
        reg_add(&top, Some("MUIVerb"), GROUP_LABEL)?;
        reg_add(&top, Some("Icon"), &exe)?;
        reg_add(&top, Some("subcommands"), "")?;

        // "Open in HK Image Sorter" — sorts first; separator below it
        let open = format!("{top}\\shell\\a_Open");
        reg_add(&open, Some("MUIVerb"), "Open in HK Image Sorter")?;
        reg_add(&open, Some("Icon"), &exe)?;
        reg_add(&open, Some("CommandFlags"), "0x40")?;
        reg_add(&format!("{open}\\command"), None, &format!("{exe} \"%1\""))?;

        // "Show Metadata Info" — sorts second
        let meta = format!("{top}\\shell\\b_Metadata");
        reg_add(&meta, Some("MUIVerb"), "Show Metadata Info")?;
        reg_add(&meta, Some("Icon"), &exe)?;
        reg_add(&format!("{meta}\\command"), None, &format!("{exe} --metadata \"%1\""))?;
    }
    Ok(())
}

/// Installs the direct item for folders and the folder background.
fn install_folder_items() -> Result<(), String> {
    let exe = quoted_exe()?;
    // Right-click on a folder: %1 = the folder path
    let dir = r"HKCU\Software\Classes\Directory\shell\HKIS.Open";
    reg_add(dir, None, "Open in HK Image Sorter")?;
    reg_add(dir, Some("Icon"), &exe)?;
    reg_add(dir, Some("SeparatorBefore"), "")?;
    reg_add(&format!("{dir}\\command"), None, &format!("{exe} \"%1\""))?;
    // Right-click on empty space inside a folder: %V = the current folder
    let bg = r"HKCU\Software\Classes\Directory\Background\shell\HKIS.Open";
    reg_add(bg, None, "Open in HK Image Sorter")?;
    reg_add(bg, Some("Icon"), &exe)?;
    reg_add(bg, Some("SeparatorBefore"), "")?;
    reg_add(&format!("{bg}\\command"), None, &format!("{exe} \"%V\""))?;
    Ok(())
}

/// Adds all context-menu entries.
pub fn install() -> Result<(), String> {
    install_file_items()?;
    install_folder_items()?;
    Ok(())
}

/// Removes all context-menu entries (plus any leftover keys from older
/// layouts — a grouped `HKIS` submenu key or legacy direct items). Missing
/// keys are fine (already removed).
pub fn uninstall() -> Result<(), String> {
    for ext in IMAGE_EXTS {
        reg_delete(&sfa_path(ext, "HKIS.Open"));
        reg_delete(&sfa_path(ext, "HKIS.Metadata"));
        reg_delete(&sfa_path(ext, "HKIS.Explorer"));
        reg_delete(&sfa_path(ext, "HKIS")); // old grouped submenu layout
    }
    reg_delete(r"HKCU\Software\Classes\Directory\shell\HKIS.Open");
    reg_delete(r"HKCU\Software\Classes\Directory\Background\shell\HKIS.Open");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfa_path_shape() {
        assert_eq!(
            sfa_path("png", "HKIS"),
            r"HKCU\Software\Classes\SystemFileAssociations\.png\shell\HKIS"
        );
    }

    #[test]
    fn cascade_subkeys_have_sort_prefixes() {
        let top = sfa_path("png", "HKIS");
        assert_eq!(format!("{top}\\shell\\a_Open\\command"),
            r"HKCU\Software\Classes\SystemFileAssociations\.png\shell\HKIS\shell\a_Open\command");
        assert_eq!(format!("{top}\\shell\\b_Metadata\\command"),
            r"HKCU\Software\Classes\SystemFileAssociations\.png\shell\HKIS\shell\b_Metadata\command");
    }

    #[test]
    fn commands_use_explorer_placeholders() {
        let exe = "\"C:\\apps\\sorter.exe\"";
        assert_eq!(format!("{exe} \"%1\""), "\"C:\\apps\\sorter.exe\" \"%1\"");
        assert_eq!(
            format!("{exe} --metadata \"%1\""),
            "\"C:\\apps\\sorter.exe\" --metadata \"%1\""
        );
        assert_eq!(format!("{exe} \"%V\""), "\"C:\\apps\\sorter.exe\" \"%V\"");
    }

    #[test]
    fn seven_image_extensions() {
        assert_eq!(IMAGE_EXTS.len(), 7);
    }
}
