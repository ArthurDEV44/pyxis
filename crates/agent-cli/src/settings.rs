use std::io;
use std::path::{Path, PathBuf};

use agent_tools::PermissionMode;

const SETTINGS_FILE: &str = "settings.toml";
const PERMISSION_MODE_KEY: &str = "permission_mode";

pub fn permission_mode_id(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "ask",
        PermissionMode::AcceptEdits => "accept-edits",
        PermissionMode::DontAsk => "auto",
        PermissionMode::BypassPermissions => "full-access",
        PermissionMode::Plan => "read-only",
    }
}

pub fn permission_mode_from_arg(arg: &str) -> Option<PermissionMode> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "ask" | "default" | "ask-for-approval" => Some(PermissionMode::Default),
        "accept-edits" | "edits" | "auto-approve-edits" => Some(PermissionMode::AcceptEdits),
        "auto" | "approve-for-me" | "dont-ask" => Some(PermissionMode::DontAsk),
        "full-access" | "full" | "bypass" | "bypass-permissions" => {
            Some(PermissionMode::BypassPermissions)
        }
        "read-only" | "readonly" | "plan" => Some(PermissionMode::Plan),
        _ => None,
    }
}

pub fn default_settings_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("PYXIS_HOME") {
        return Some(PathBuf::from(root).join(SETTINGS_FILE));
    }
    home_dir().map(|home| home.join(".pyxis").join(SETTINGS_FILE))
}

pub fn load_permission_mode(path: &Path) -> io::Result<Option<PermissionMode>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };

    Ok(contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != PERMISSION_MODE_KEY {
            return None;
        }
        let value = parse_tomlish_string(value.trim())?;
        permission_mode_from_arg(value)
    }))
}

pub fn save_permission_mode(path: &Path, mode: PermissionMode) -> io::Result<()> {
    let value = permission_mode_id(mode);
    let new_line = format!("{PERMISSION_MODE_KEY} = \"{value}\"");
    let mut replaced = false;
    let mut lines = match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .map(|line| {
                if is_permission_mode_line(line) && !replaced {
                    replaced = true;
                    new_line.clone()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };

    if !replaced {
        lines.push(new_line);
    }
    let mut contents = lines.join("\n");
    contents.push('\n');

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

fn is_permission_mode_line(line: &str) -> bool {
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    key.trim() == PERMISSION_MODE_KEY
}

fn parse_tomlish_string(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split_once('"').map(|(value, _)| value);
    }
    value
        .split('#')
        .next()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pyxis-settings-{}-{tag}.toml", std::process::id()))
    }

    #[test]
    fn load_permission_mode_reads_saved_value() {
        let path = temp_path("load");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "permission_mode = \"accept-edits\" # keep\n").unwrap();

        let mode = load_permission_mode(&path).unwrap();

        assert_eq!(mode, Some(PermissionMode::AcceptEdits));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_permission_mode_creates_file() {
        let path = temp_path("create");
        let _ = std::fs::remove_file(&path);

        save_permission_mode(&path, PermissionMode::Plan).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "permission_mode = \"read-only\"\n"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_permission_mode_replaces_existing_key_and_preserves_other_lines() {
        let path = temp_path("replace");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "model = \"gpt\"\npermission_mode = \"ask\"\n").unwrap();

        save_permission_mode(&path, PermissionMode::BypassPermissions).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "model = \"gpt\"\npermission_mode = \"full-access\"\n"
        );
        let _ = std::fs::remove_file(path);
    }
}
