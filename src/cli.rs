//! Network operations run through the `git` command line in a background thread.
//!
//! Using the installed `git` means clone/fetch/pull/push reuse whatever the user
//! already has configured: Git Credential Manager, the macOS keychain, SSH agents,
//! proxies and so on.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetOp {
    Clone { url: String, dest: PathBuf },
    Fetch,
    Pull,
    Push { set_upstream: Option<(String, String)> },
}

impl NetOp {
    pub fn label(&self) -> &'static str {
        match self {
            NetOp::Clone { .. } => "Cloning",
            NetOp::Fetch => "Fetching",
            NetOp::Pull => "Pulling",
            NetOp::Push { .. } => "Pushing",
        }
    }
}

pub struct NetResult {
    pub op: NetOp,
    /// The repository the operation ran in (or the clone destination).
    pub repo: PathBuf,
    pub result: Result<String, String>,
}

/// Is a `git` executable available on PATH?
pub fn git_available() -> bool {
    command(None).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn command(cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // Never block waiting for a password on a terminal nobody can see.
    // Graphical credential helpers (Git Credential Manager, keychain) still work.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run(cwd: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let output = command(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("Could not run git: {e}. Is Git installed and on your PATH?"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{}{}", stdout, stderr).trim().to_string();
    if output.status.success() {
        Ok(text)
    } else {
        Err(friendly_error(&text))
    }
}

fn friendly_error(text: &str) -> String {
    let lower = text.to_lowercase();
    let hint = if lower.contains("terminal prompts disabled")
        || lower.contains("authentication failed")
        || lower.contains("could not read username")
    {
        Some(
            "Authentication is needed. Set up a credential helper (for example Git Credential \
             Manager on Windows, or the keychain on macOS), or use an SSH remote URL.",
        )
    } else if lower.contains("permission denied (publickey)") {
        Some("Your SSH key was rejected. Check that it is added to your Git host and ssh-agent.")
    } else if lower.contains("non-fast-forward") || lower.contains("fetch first") {
        Some("The remote has commits you don't have yet. Pull first, then push again.")
    } else if lower.contains("would be overwritten") {
        Some("Commit or discard your local changes first, then try again.")
    } else {
        None
    };
    let text = if text.is_empty() { "git failed without any output" } else { text };
    match hint {
        Some(h) => format!("{h}\n\n{text}"),
        None => text.to_string(),
    }
}

/// Run a network operation on a background thread and send the result back.
pub fn spawn(op: NetOp, repo: PathBuf, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let result = match &op {
            NetOp::Clone { url, dest } => {
                let dest_s = dest.to_string_lossy().to_string();
                run(None, &["clone", "--progress", url, &dest_s])
            }
            NetOp::Fetch => run(Some(&repo), &["fetch", "--all", "--prune"]),
            NetOp::Pull => run(Some(&repo), &["pull", "--no-rebase", "--no-edit"]),
            NetOp::Push { set_upstream } => match set_upstream {
                Some((remote, branch)) => run(Some(&repo), &["push", "-u", remote, branch]),
                None => run(Some(&repo), &["push"]),
            },
        };
        let _ = tx.send(NetResult { op, repo, result });
        ctx.request_repaint();
    });
}

/// Guess a folder name from a clone URL, e.g. `https://host/user/project.git` -> `project`.
pub fn name_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    trimmed
        .rsplit(['/', ':', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("repository")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_urls() {
        assert_eq!(name_from_url("https://github.com/profawie/gitsot.git"), "gitsot");
        assert_eq!(name_from_url("https://github.com/profawie/gitsot/"), "gitsot");
        assert_eq!(name_from_url("git@github.com:profawie/gitsot.git"), "gitsot");
        assert_eq!(name_from_url("gitsot"), "gitsot");
    }
}
