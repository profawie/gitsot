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
    /// Delete `branch` on `remote` (`git push <remote> --delete <branch>`).
    DeleteRemoteBranch { remote: String, branch: String },
    /// Merge `branch` into the current branch (`git merge --no-edit`).
    Merge { branch: String },
    /// Stop a merge that has conflicts (`git merge --abort`).
    MergeAbort,
}

impl NetOp {
    pub fn verb(&self) -> &'static str {
        match self {
            NetOp::Clone { .. } => "clone",
            NetOp::Fetch => "fetch",
            NetOp::Pull => "pull",
            NetOp::Push { .. } => "push",
            NetOp::DeleteRemoteBranch { .. } => "delete the branch",
            NetOp::Merge { .. } => "merge",
            NetOp::MergeAbort => "abort the merge",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            NetOp::Clone { .. } => "Cloning",
            NetOp::Fetch => "Fetching",
            NetOp::Pull => "Pulling",
            NetOp::Push { .. } => "Pushing",
            NetOp::DeleteRemoteBranch { .. } => "Deleting remote branch",
            NetOp::Merge { .. } => "Merging",
            NetOp::MergeAbort => "Aborting the merge",
        }
    }
}

pub struct NetResult {
    pub op: NetOp,
    /// The repository the operation ran in (or the clone destination).
    pub repo: PathBuf,
    pub result: Result<String, String>,
    /// The remote asked for a username/password that nobody supplied.
    pub auth_failed: bool,
    /// Credentials from Gitsot's sign-in dialog were used for this attempt.
    pub used_credentials: bool,
}

/// Username and password (or access token) typed into Gitsot's own sign-in dialog.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// A credential helper that answers `get` with the values Gitsot passes in
/// environment variables, so the secret never appears on a command line.
/// It is added *after* the user's own helpers, so a saved credential still wins,
/// and when the sign-in succeeds git asks those helpers to store it.
const INLINE_HELPER: &str = "credential.helper=!f() { test \"$1\" = get && \
printf 'username=%s\\npassword=%s\\n' \"$GITSOT_GIT_USERNAME\" \"$GITSOT_GIT_PASSWORD\"; }; f";

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
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // Never let a credential helper open its own window (for example Git Credential
    // Manager's host-specific sign-in pages). Saved credentials are still used;
    // when none exist Gitsot shows its own host-neutral sign-in dialog instead.
    cmd.env("GCM_INTERACTIVE", "never");
    cmd.env("GCM_GUI_PROMPT", "false");
    cmd.env_remove("GIT_ASKPASS");
    cmd.env_remove("SSH_ASKPASS");
    cmd.env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run(cwd: Option<&Path>, args: &[&str], creds: Option<&Credentials>) -> Result<String, String> {
    let mut cmd = command(cwd);
    if let Some(c) = creds {
        cmd.args(["-c", INLINE_HELPER]);
        cmd.env("GITSOT_GIT_USERNAME", &c.username);
        cmd.env("GITSOT_GIT_PASSWORD", &c.password);
    }
    let output = cmd
        .args(args)
        .output()
        .map_err(|e| format!("Could not run git: {e}. Is Git installed and on your PATH?"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{}{}", stdout, stderr).trim().to_string();
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// Did git fail because the remote wants a username/password we could not supply?
pub fn is_auth_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "terminal prompts disabled",
        "could not read username",
        "could not read password",
        "authentication failed",
        "user interactivity has been disabled",
        "invalid username or password",
        "access denied",
        "returned error: 401",
        "returned error: 403",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Is this an http(s) URL (where a username/password dialog makes sense)?
pub fn is_http_url(url: &str) -> bool {
    let u = url.trim().to_lowercase();
    u.starts_with("https://") || u.starts_with("http://")
}

/// `https://user@host:443/path` -> `host`
pub fn host_of(url: &str) -> String {
    let rest = url.trim().split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.split(':').next().unwrap_or(host).to_string()
}

pub fn friendly_error(text: &str) -> String {
    let lower = text.to_lowercase();
    let hint = if is_auth_error(text) {
        Some("The remote rejected the sign-in. Check your username and password or access token.")
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
/// `wake` is called after the result is sent, so the UI can redraw.
pub fn spawn(
    op: NetOp,
    repo: PathBuf,
    creds: Option<Credentials>,
    tx: Sender<NetResult>,
    wake: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        let c = creds.as_ref();
        let result = match &op {
            NetOp::Clone { url, dest } => {
                let dest_s = dest.to_string_lossy().to_string();
                run(None, &["clone", "--progress", url, &dest_s], c)
            }
            NetOp::Fetch => run(Some(&repo), &["fetch", "--all", "--prune"], c),
            NetOp::Pull => run(Some(&repo), &["pull", "--no-rebase", "--no-edit"], c),
            NetOp::Push { set_upstream } => match set_upstream {
                Some((remote, branch)) => run(Some(&repo), &["push", "-u", remote, branch], c),
                None => run(Some(&repo), &["push"], c),
            },
            NetOp::DeleteRemoteBranch { remote, branch } => run(Some(&repo), &["push", remote, "--delete", branch], c),
            NetOp::Merge { branch } => run(Some(&repo), &["merge", "--no-edit", branch], c),
            NetOp::MergeAbort => run(Some(&repo), &["merge", "--abort"], c),
        };
        let auth_failed = matches!(&result, Err(e) if is_auth_error(e));
        let result = result.map_err(|e| friendly_error(&e));
        let used_credentials = creds.is_some();
        let _ = tx.send(NetResult { op, repo, result, auth_failed, used_credentials });
        wake();
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

    #[test]
    fn hosts_and_auth_errors() {
        assert_eq!(host_of("https://gitlab.example.com:8443/a/b.git"), "gitlab.example.com");
        assert_eq!(host_of("https://me@bitbucket.org/a/b.git"), "bitbucket.org");
        assert!(is_http_url("HTTPS://x/y"));
        assert!(!is_http_url("git@github.com:a/b.git"));
        assert!(is_auth_error("fatal: could not read Username for 'https://x': terminal prompts disabled"));
        assert!(is_auth_error("fatal: Authentication failed for 'https://x/'"));
        assert!(!is_auth_error("fatal: not a git repository"));
    }
}
