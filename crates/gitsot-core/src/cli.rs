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
    /// What a successful Fetch did to the local branches.
    pub updates: Option<BranchUpdates>,
}

/// What Fetch did to the local branches after downloading from the remotes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchUpdates {
    /// Branches moved forward to their upstream, with the number of new commits.
    pub updated: Vec<(String, usize)>,
    /// Branches left alone because they have commits that are not pushed yet
    /// while the remote has new ones: `(name, unpushed, waiting)`.
    pub unpushed: Vec<(String, usize, usize)>,
    /// Branches that could not be moved, with git's reason.
    pub failed: Vec<(String, String)>,
}

impl BranchUpdates {
    pub fn is_clean(&self) -> bool {
        self.unpushed.is_empty() && self.failed.is_empty()
    }
}

/// `[ahead 1, behind 2]` -> `(1, 2)`. `None` when the upstream is gone.
fn parse_track(track: &str) -> Option<(usize, usize)> {
    let inner = track.trim().trim_start_matches('[').trim_end_matches(']');
    if inner == "gone" {
        return None;
    }
    let count = |key: &str| {
        inner
            .split(',')
            .find_map(|part| part.trim().strip_prefix(key))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    Some((count("ahead"), count("behind")))
}

/// Bring every local branch that follows a remote branch up to date, without
/// ever touching the network: this only moves branches to commits that the
/// fetch before it already downloaded.
///
/// A branch is moved only when that is a fast-forward, so nothing is merged
/// and no commit can be lost. A branch with commits that are not pushed yet is
/// left alone and reported. The current branch is fast-forwarded with
/// `git merge --ff-only`, which refuses when it would overwrite local changes;
/// the others with `git fetch . <upstream>:<branch>`, which refuses anything
/// that is not a fast-forward and any branch checked out in another worktree.
pub fn update_branches(repo: &Path) -> BranchUpdates {
    let mut out = BranchUpdates::default();
    let format = "--format=%(refname)%09%(upstream)%09%(HEAD)%09%(upstream:track)";
    let Ok(list) = run(Some(repo), &["for-each-ref", format, "refs/heads"], None) else {
        return out;
    };
    for line in list.lines() {
        let mut cols = line.split('\t');
        let (Some(refname), Some(upstream), Some(head)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let Some(name) = refname.strip_prefix("refs/heads/") else { continue };
        if upstream.is_empty() {
            continue;
        }
        let Some((ahead, behind)) = parse_track(cols.next().unwrap_or("")) else {
            continue;
        };
        if behind == 0 {
            continue;
        }
        if ahead > 0 {
            out.unpushed.push((name.to_string(), ahead, behind));
            continue;
        }
        let result = if head.trim() == "*" {
            run(Some(repo), &["merge", "--ff-only", "--quiet", upstream], None)
        } else {
            let refspec = format!("{upstream}:{refname}");
            let args = ["fetch", "--quiet", "--no-tags", "--no-prune", "--no-recurse-submodules", ".", &refspec];
            run(Some(repo), &args, None)
        };
        match result {
            Ok(_) => out.updated.push((name.to_string(), behind)),
            Err(e) => out.failed.push((name.to_string(), e)),
        }
    }
    out
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

/// Ask Git's credential helpers for the sign-in saved for `url`, without ever
/// prompting. `None` when nothing is saved (or no helper is configured).
pub fn credential_fill(repo: &Path, url: &str) -> Option<Credentials> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command(Some(repo))
        .args(["credential", "fill"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let url: String = url.trim().chars().filter(|c| !c.is_control()).collect();
    child.stdin.take()?.write_all(format!("url={url}\n\n").as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::to_string);
    let password = value("password=").filter(|p| !p.is_empty())?;
    Some(Credentials { username: value("username=").unwrap_or_default(), password })
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
        // Fetch also brings the local branches up to date with what it downloaded.
        let updates = (op == NetOp::Fetch && result.is_ok()).then(|| update_branches(&repo));
        let result = result.map_err(|e| friendly_error(&e));
        let used_credentials = creds.is_some();
        let _ = tx.send(NetResult { op, repo, result, auth_failed, used_credentials, updates });
        wake();
    });
}

/// How long a background check may take before it is stopped.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Result of a background check for new commits on the remote.
pub struct CheckResult {
    pub repo: PathBuf,
    pub result: Result<(), String>,
    /// The remote wants a sign-in that is not saved anywhere.
    pub auth_failed: bool,
}

/// Fetch from the remotes of `repo` on a background thread, without ever
/// asking the user anything.
///
/// It is the same `git fetch` as the Fetch button, run inside the repository,
/// so Git picks the identity that repository is set up with: its own
/// credential helpers, `includeIf` sections, SSH command and keys. On top of
/// that this never passes credentials typed into Gitsot's sign-in dialog,
/// never lets a helper or SSH prompt, and stops after a minute. When no saved
/// sign-in works it fails quietly and reports `auth_failed`.
pub fn spawn_check(repo: PathBuf, tx: Sender<CheckResult>, wake: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let result = check(&repo);
        let auth_failed = matches!(&result, Err(e) if is_auth_error(e) || e.to_lowercase().contains("permission denied (publickey"));
        let _ = tx.send(CheckResult { repo, result, auth_failed });
        wake();
    });
}

fn check(repo: &Path) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut cmd = command(Some(repo));
    // Stop SSH from asking for a passphrase or a host key on a terminal the
    // app was started from. Only when the user has not chosen their own SSH
    // command: that choice may be what selects the right key for this
    // repository, and it must win.
    let own_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
        || std::env::var_os("GIT_SSH").is_some()
        || run(Some(repo), &["config", "--get", "core.sshCommand"], None).is_ok_and(|v| !v.is_empty());
    if !own_ssh {
        cmd.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    }
    let mut child = cmd
        .args(["fetch", "--all", "--prune", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run git: {e}"))?;
    // Read errors on another thread, so a helper that outlives git and keeps
    // the pipe open cannot hold the check up.
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let _ = err_tx.send(text);
        });
    }
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > CHECK_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The remote did not answer within a minute.".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(format!("Could not run git: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let text = err_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default();
    let text = text.trim();
    Err(if text.is_empty() { "git fetch failed without any output".to_string() } else { text.to_string() })
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
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn background_check_fetches_quietly_and_reports_failures() {
        if !git_available() {
            return;
        }
        let root = std::env::temp_dir().join(format!("gitsot-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (origin, work, clone) = (root.join("origin.git"), root.join("work"), root.join("clone"));
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "--bare", "-q"]);
        git(&root, &["clone", "-q", "origin.git", "work"]);
        let commit = |n: &str| {
            std::fs::write(work.join("a.txt"), n).unwrap();
            git(&work, &["add", "a.txt"]);
            git(&work, &["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "-m", n]);
            git(&work, &["push", "-q", "origin", "HEAD"]);
        };
        commit("one");
        git(&root, &["clone", "-q", "origin.git", "clone"]);
        commit("two");

        let (tx, rx) = std::sync::mpsc::channel();
        spawn_check(clone.clone(), tx.clone(), || {});
        let res = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(res.result, Ok(()));
        assert!(!res.auth_failed);
        // The new commit is known, and the branch itself was not moved.
        let behind = Command::new("git").current_dir(&clone).args(["rev-list", "--count", "HEAD..@{u}"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&behind.stdout).trim(), "1");
        assert!(clone.join(".git/FETCH_HEAD").exists());

        // A remote that is gone fails without asking anything.
        git(&clone, &["remote", "set-url", "origin", root.join("gone.git").to_str().unwrap()]);
        spawn_check(clone.clone(), tx, || {});
        let res = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert!(res.result.is_err());
        assert!(!res.auth_failed);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn track_is_parsed() {
        assert_eq!(parse_track(""), Some((0, 0)));
        assert_eq!(parse_track("[ahead 3]"), Some((3, 0)));
        assert_eq!(parse_track("[behind 12]"), Some((0, 12)));
        assert_eq!(parse_track("[ahead 1, behind 2]"), Some((1, 2)));
        assert_eq!(parse_track("[gone]"), None);
    }

    #[test]
    fn fetch_updates_every_branch_but_never_one_with_unpushed_commits() {
        if !git_available() {
            return;
        }
        let root = std::env::temp_dir().join(format!("gitsot-fetch-all-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (origin, work, clone) = (root.join("origin.git"), root.join("work"), root.join("clone"));
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "--bare", "-q", "-b", "main"]);
        git(&root, &["clone", "-q", "origin.git", "work"]);
        let commit_in = |dir: &Path, file: &str, text: &str| {
            std::fs::write(dir.join(file), text).unwrap();
            git(dir, &["add", file]);
            git(dir, &["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "-m", text]);
        };
        let rev = |dir: &Path, name: &str| {
            let out = Command::new("git").current_dir(dir).args(["rev-parse", name]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&work, &["checkout", "-q", "-b", "main"]);
        commit_in(&work, "a.txt", "one");
        git(&work, &["push", "-q", "origin", "main"]);
        for b in ["feature", "mine", "current", "dirty"] {
            git(&work, &["branch", b]);
            git(&work, &["push", "-q", "origin", b]);
        }
        git(&root, &["clone", "-q", "origin.git", "clone"]);
        for b in ["feature", "mine", "current", "dirty"] {
            git(&clone, &["checkout", "-q", b]);
        }
        // "mine" has a commit that is not pushed; "current" is checked out.
        git(&clone, &["checkout", "-q", "mine"]);
        commit_in(&clone, "mine.txt", "local work");
        let mine_before = rev(&clone, "mine");
        git(&clone, &["checkout", "-q", "current"]);
        // Somebody else pushes to every branch (two commits to main).
        for (b, file) in [("main", "a.txt"), ("main", "a.txt"), ("feature", "f.txt"), ("mine", "m.txt"), ("current", "c.txt")] {
            git(&work, &["checkout", "-q", b]);
            commit_in(&work, file, &format!("new on {b} {}", rev(&work, "HEAD")));
            git(&work, &["push", "-q", "origin", b]);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        spawn(NetOp::Fetch, clone.clone(), None, tx.clone(), || {});
        let res = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert!(res.result.is_ok(), "{:?}", res.result);
        let updates = res.updates.unwrap();
        assert_eq!(
            updates.updated,
            vec![("current".to_string(), 1), ("feature".to_string(), 1), ("main".to_string(), 2)]
        );
        assert_eq!(updates.unpushed, vec![("mine".to_string(), 1, 1)]);
        assert!(updates.failed.is_empty(), "{:?}", updates.failed);
        for b in ["main", "feature", "current"] {
            assert_eq!(rev(&clone, b), rev(&clone, &format!("origin/{b}")), "{b}");
        }
        // The checked-out branch got its files too, and "mine" was not touched.
        assert!(clone.join("c.txt").exists());
        assert_eq!(rev(&clone, "mine"), mine_before);
        assert_eq!(rev(&clone, "HEAD"), rev(&clone, "current"));

        // A local change that the new commit would overwrite stops the current
        // branch from moving, and is reported instead of lost.
        git(&clone, &["checkout", "-q", "dirty"]);
        git(&work, &["checkout", "-q", "dirty"]);
        commit_in(&work, "a.txt", "theirs");
        git(&work, &["push", "-q", "origin", "dirty"]);
        std::fs::write(clone.join("a.txt"), "my unsaved work").unwrap();
        let dirty_before = rev(&clone, "dirty");
        spawn(NetOp::Fetch, clone.clone(), None, tx, || {});
        let updates = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().updates.unwrap();
        assert_eq!(updates.failed.len(), 1, "{updates:?}");
        assert_eq!(updates.failed[0].0, "dirty");
        assert_eq!(rev(&clone, "dirty"), dirty_before);
        assert_eq!(std::fs::read_to_string(clone.join("a.txt")).unwrap(), "my unsaved work");

        let _ = std::fs::remove_dir_all(&root);
    }
}
