//! Which shell the built-in terminal starts, and how Gitsot's prompt is
//! loaded into it without touching the user's own startup files.
//!
//! * Windows: PowerShell (`pwsh` if installed, else Windows PowerShell).
//! * macOS: zsh, as a login shell like Terminal.app.
//! * Linux: the user's zsh or bash.

use std::{
    collections::HashMap,
    path::Path,
};

#[cfg(any(windows, test))]
const PROMPT_PS1: &str = include_str!("../assets/prompt.ps1");
#[cfg(any(unix, test))]
const PROMPT_SH: &str = include_str!("../assets/prompt.sh");

/// Everything needed to start the shell.
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Name for display, e.g. "PowerShell".
    pub name: String,
}

/// The shell for a terminal opened in `repo`. With `rich_prompt`, Gitsot's
/// prompt (labelled chips for branch, changes, identity…) is loaded after the
/// user's own profile.
pub fn launch(repo: &Path, rich_prompt: bool) -> Launch {
    let mut env = HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "Gitsot".into());
    env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into());
    env.insert("GITSOT_REPO".into(), repo.display().to_string());
    platform_launch(env, rich_prompt)
}

#[cfg(windows)]
fn platform_launch(env: HashMap<String, String>, rich_prompt: bool) -> Launch {
    let (program, name) = if on_path("pwsh.exe") {
        ("pwsh.exe", "PowerShell")
    } else {
        ("powershell.exe", "Windows PowerShell")
    };
    let mut args = vec!["-NoLogo".to_string()];
    if rich_prompt {
        // Passed as an encoded command: no file to write, nothing to quote,
        // and it works whatever the script execution policy is.
        args.extend(["-NoExit".to_string(), "-EncodedCommand".to_string(), encoded_command(&minified(PROMPT_PS1))]);
    }
    Launch { program: program.into(), args, env, name: name.into() }
}

#[cfg(windows)]
fn on_path(file: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(file).is_file()))
}

#[cfg(unix)]
fn platform_launch(mut env: HashMap<String, String>, rich_prompt: bool) -> Launch {
    // Apps started from the Dock or a launcher often have no locale; without
    // one the shell cannot show anything but ASCII.
    if ["LC_ALL", "LC_CTYPE", "LANG"].iter().all(|key| std::env::var_os(key).is_none_or(|v| v.is_empty())) {
        let locale = if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" };
        env.insert("LANG".into(), locale.into());
    }

    let shell = pick_unix_shell();
    let kind = shell.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let mut args = Vec::new();
    if cfg!(target_os = "macos") {
        // A login shell, like Terminal.app, so PATH from ~/.zprofile
        // (Homebrew etc.) is there even when Gitsot was started from the Dock.
        args.push("-l".to_string());
    }
    if rich_prompt {
        match (kind.as_str(), write_support_files()) {
            ("zsh", Some(dir)) => {
                // zsh reads its startup files from ZDOTDIR. Ours load the
                // user's own files first, then the prompt.
                let user_dir = std::env::var("ZDOTDIR").unwrap_or_default();
                env.insert("GITSOT_USER_ZDOTDIR".into(), user_dir);
                env.insert("GITSOT_PROMPT".into(), dir.join("prompt.sh").display().to_string());
                env.insert("ZDOTDIR".into(), dir.join("zsh").display().to_string());
            }
            ("bash", Some(dir)) => {
                env.insert("GITSOT_PROMPT".into(), dir.join("prompt.sh").display().to_string());
                args.extend(["--rcfile".to_string(), dir.join("bashrc").display().to_string()]);
            }
            _ => {}
        }
    }
    Launch { program: shell.display().to_string(), args, env, name: kind }
}

#[cfg(unix)]
fn pick_unix_shell() -> std::path::PathBuf {
    use std::path::PathBuf;
    let known = |path: &Path| {
        path.is_file() && matches!(path.file_name().and_then(|n| n.to_str()), Some("zsh") | Some("bash"))
    };
    let preferred: &[&str] = if cfg!(target_os = "macos") {
        &["/bin/zsh", "/bin/bash"]
    } else {
        &["/bin/zsh", "/usr/bin/zsh", "/bin/bash", "/usr/bin/bash"]
    };
    // macOS always uses zsh. Elsewhere the user's own shell comes first.
    if !cfg!(target_os = "macos") {
        if let Some(shell) = std::env::var_os("SHELL").map(PathBuf::from).filter(|p| known(p)) {
            return shell;
        }
    }
    preferred.iter().map(PathBuf::from).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// zsh startup files that hand over to the user's own, then load the prompt.
#[cfg(unix)]
const ZSH_FILES: [(&str, &str); 3] = [
    (
        ".zshenv",
        r#"# Written by Gitsot. Loads your own .zshenv.
ZDOTDIR="$GITSOT_USER_ZDOTDIR"
[ -f "${ZDOTDIR:-$HOME}/.zshenv" ] && . "${ZDOTDIR:-$HOME}/.zshenv"
GITSOT_USER_ZDOTDIR="$ZDOTDIR"
ZDOTDIR="${GITSOT_PROMPT:h}/zsh"
"#,
    ),
    (
        ".zprofile",
        r#"# Written by Gitsot. Loads your own .zprofile.
[ -f "${GITSOT_USER_ZDOTDIR:-$HOME}/.zprofile" ] && . "${GITSOT_USER_ZDOTDIR:-$HOME}/.zprofile"
"#,
    ),
    (
        ".zshrc",
        r#"# Written by Gitsot. Loads your own .zshrc, then Gitsot's prompt.
if [ -n "$GITSOT_USER_ZDOTDIR" ]; then ZDOTDIR="$GITSOT_USER_ZDOTDIR"; else unset ZDOTDIR; fi
unset GITSOT_USER_ZDOTDIR
[ -f "${ZDOTDIR:-$HOME}/.zshrc" ] && . "${ZDOTDIR:-$HOME}/.zshrc"
[ -f "$GITSOT_PROMPT" ] && . "$GITSOT_PROMPT"
"#,
    ),
];

#[cfg(unix)]
const BASHRC: &str = r#"# Written by Gitsot. Loads your own .bashrc, then Gitsot's prompt.
[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
[ -f "$GITSOT_PROMPT" ] && . "$GITSOT_PROMPT"
"#;

/// Write the prompt and the startup files into Gitsot's own data folder.
/// Returns the folder, or `None` if it cannot be written (the terminal then
/// starts with the plain shell).
#[cfg(unix)]
fn write_support_files() -> Option<std::path::PathBuf> {
    let dir = eframe::storage_dir("gitsot")?.join("terminal");
    std::fs::create_dir_all(dir.join("zsh")).ok()?;
    write_if_changed(&dir.join("prompt.sh"), PROMPT_SH)?;
    write_if_changed(&dir.join("bashrc"), BASHRC)?;
    for (name, text) in ZSH_FILES {
        write_if_changed(&dir.join("zsh").join(name), text)?;
    }
    Some(dir)
}

#[cfg(unix)]
fn write_if_changed(path: &Path, text: &str) -> Option<()> {
    if std::fs::read_to_string(path).is_ok_and(|current| current == text) {
        return Some(());
    }
    std::fs::write(path, text).ok()
}

/// Drop comment lines and indentation, to keep the command line short.
#[cfg(any(windows, test))]
fn minified(script: &str) -> String {
    script.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect::<Vec<_>>().join("\n")
}

/// PowerShell's `-EncodedCommand` format: Base64 of the UTF-16LE text.
#[cfg(any(windows, test))]
fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

#[cfg(any(windows, test))]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encoded_command_is_utf16_little_endian() {
        // "dir" as UTF-16LE: 64 00 69 00 72 00
        assert_eq!(encoded_command("dir"), "ZABpAHIA");
    }

    #[test]
    fn powershell_prompt_fits_on_a_windows_command_line() {
        let script = minified(PROMPT_PS1);
        assert!(script.is_ascii(), "prompt.ps1 must stay plain ASCII");
        assert!(!script.lines().any(|line| line.starts_with('#')));
        assert!(script.contains("function global:prompt"));
        // CreateProcess accepts at most 32,767 characters.
        assert!(encoded_command(&script).len() < 30_000);
    }

    #[test]
    fn shell_prompt_sets_up_zsh_and_bash() {
        assert!(PROMPT_SH.contains("add-zsh-hook precmd __gitsot_precmd"));
        assert!(PROMPT_SH.contains("PROMPT_COMMAND=__gitsot_precmd"));
    }
}
