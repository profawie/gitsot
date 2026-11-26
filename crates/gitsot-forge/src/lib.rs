//! Pull requests on the Git host behind a remote (a "forge").
//!
//! gitsot is not tied to one host, so everything the UI needs goes through
//! the [`Provider`] trait: list pull requests, read one (description, files,
//! reviews) and submit a review. GitHub (github.com and GitHub Enterprise) is
//! implemented in this version; other hosts are recognised and reported as
//! not supported yet, so adding one means adding one `Provider`.
//!
//! Requests are sent with the `curl` command that ships with Windows and
//! macOS, the same way network Git operations use the installed `git`. That
//! keeps the system certificate store and proxy settings working. Secrets are
//! passed on curl's standard input, never on a command line.
//!
//! Built as a dynamic library: `gitsot_forge.dll` on Windows,
//! `libgitsot_forge.dylib` on macOS and `libgitsot_forge.so` on Linux.

// Use the third-party libraries inside gitsot_shared instead of linking our
// own copies (see crates/gitsot-shared).
extern crate gitsot_shared;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use gitsot_core::cli::{self, Credentials};
use gitsot_core::git::{ChangeKind, DiffLine, FileDiff, LineKind};

/// Most files read for one pull request (three pages of 100).
pub const MAX_FILES: usize = 300;
/// Pull requests shown in the list.
pub const LIST_LIMIT: usize = 50;

// ---------------------------------------------------------------------------
// Which host is this?
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    GitHub,
    GitLab,
    Gitea,
    Bitbucket,
    Unknown,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::GitHub => "GitHub",
            Kind::GitLab => "GitLab",
            Kind::Gitea => "Gitea / Forgejo",
            Kind::Bitbucket => "Bitbucket",
            Kind::Unknown => "this host",
        }
    }

    fn guess(host: &str) -> Kind {
        let h = host.to_lowercase();
        if h == "github.com" || h.starts_with("github.") {
            Kind::GitHub
        } else if h.contains("gitlab") {
            Kind::GitLab
        } else if h.contains("bitbucket") {
            Kind::Bitbucket
        } else if h.contains("gitea") || h.contains("forgejo") || h == "codeberg.org" {
            Kind::Gitea
        } else {
            Kind::Unknown
        }
    }
}

/// The repository a remote URL points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub kind: Kind,
    pub host: String,
    /// Owner, organisation or group path (`a/b` for nested GitLab groups).
    pub owner: String,
    pub name: String,
    /// The remote is `http(s)://`, so Git's saved sign-in for it can be used.
    pub http: bool,
}

impl Remote {
    /// Understands `https://host/owner/name.git`, `ssh://git@host/owner/name`
    /// and `git@host:owner/name.git`. Local paths give `None`.
    pub fn parse(url: &str) -> Option<Remote> {
        let url = url.trim();
        let (host, path, http) = if let Some((scheme, rest)) = url.split_once("://") {
            let scheme = scheme.to_lowercase();
            if scheme == "file" {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?.split(':').next()?;
            (host, path, scheme == "http" || scheme == "https")
        } else {
            // scp-like: [user@]host:path. A drive letter (C:\x) is not a host.
            let (left, path) = url.split_once(':')?;
            let host = left.rsplit('@').next()?;
            if host.len() < 2 || host.contains(['/', '\\']) || path.starts_with('\\') {
                return None;
            }
            (host, path, false)
        };
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.rsplit_once('/')?;
        if host.is_empty() || owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(Remote {
            kind: Kind::guess(host),
            host: host.to_lowercase(),
            owner: owner.to_string(),
            name: name.to_string(),
            http,
        })
    }

    /// `host/owner/name`, for display.
    pub fn display(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }

    /// The provider for this host, if gitsot has one.
    pub fn provider(&self) -> Option<Box<dyn Provider>> {
        match self.kind {
            Kind::GitHub => Some(Box::new(GitHub::new(self))),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// What the UI shows
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Open,
    Closed,
    All,
}

#[derive(Clone, Debug)]
pub struct PullSummary {
    pub number: u64,
    pub title: String,
    pub author: String,
    /// Branch with the changes.
    pub head: String,
    /// Branch the changes go into.
    pub base: String,
    pub draft: bool,
    pub state: PullState,
    /// Last update, seconds since the Unix epoch.
    pub updated: i64,
    /// Page of the pull request on the host.
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct PullFile {
    pub path: String,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub diff: FileDiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// One entry of the conversation: a review or a plain comment.
#[derive(Clone, Debug)]
pub struct Note {
    pub author: String,
    /// `None` for a plain comment.
    pub review: Option<ReviewState>,
    pub body: String,
    pub time: i64,
}

#[derive(Clone, Debug)]
pub struct PullDetail {
    pub summary: PullSummary,
    pub body: String,
    pub additions: u32,
    pub deletions: u32,
    pub files: Vec<PullFile>,
    /// The pull request has more files than gitsot read.
    pub files_truncated: bool,
    /// Reviews and comments, oldest first.
    pub notes: Vec<Note>,
    /// The account the requests were made as, if signed in.
    pub viewer: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
            ReviewEvent::Comment => "Comment",
        }
    }

    /// The host refuses these without a written comment.
    pub fn needs_body(self) -> bool {
        !matches!(self, ReviewEvent::Approve)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Not signed in, or the sign-in was rejected.
    Auth,
    /// The host says the repository or pull request does not exist. Private
    /// repositories answer this way to accounts without access.
    NotFound,
    Other,
}

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    fn other(message: impl Into<String>) -> Self {
        Error { kind: ErrorKind::Other, message: message.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One Git host's pull request API.
pub trait Provider: Send {
    fn list(&self, http: &Http, filter: Filter) -> Result<Vec<PullSummary>>;
    fn detail(&self, http: &Http, number: u64) -> Result<PullDetail>;
    /// Submit a review as the signed-in account.
    fn review(&self, http: &Http, number: u64, event: ReviewEvent, body: &str) -> Result<()>;
    /// The account `http` is signed in as.
    fn viewer(&self, http: &Http) -> Result<String>;
    /// Page listing the pull requests on the host.
    fn web_url(&self) -> String;
}

/// The sign-in Git has saved for an `http(s)` remote, if any. This is the
/// account that pushes to the repository, so reviews are made as the same one.
/// SSH remotes have no such sign-in: the user gives a token instead.
pub fn saved_sign_in(repo: &Path, remote_url: &str) -> Option<Credentials> {
    if !cli::is_http_url(remote_url) {
        return None;
    }
    cli::credential_fill(repo, remote_url)
}

// ---------------------------------------------------------------------------
// HTTP through curl
// ---------------------------------------------------------------------------

/// Sends requests, with a token when there is one.
pub struct Http {
    pub token: Option<String>,
}

pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// Is a `curl` executable available on PATH?
pub fn curl_available() -> bool {
    curl_command().arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn curl_command() -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Quote a value for a curl config file.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The curl config for one request. Header values lose line breaks so a
/// value can never add a header of its own.
fn curl_config(method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> String {
    let mut config = String::new();
    let mut line = |key: &str, value: &str| {
        config.push_str(key);
        config.push_str(" = ");
        config.push_str(&quote(value));
        config.push('\n');
    };
    line("url", url);
    line("request", method);
    // HTTPS only, and no redirects: a token must never follow one to another host.
    line("proto", "=https");
    for (name, value) in headers {
        let value: String = value.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
        line("header", &format!("{name}: {value}"));
    }
    if let Some(body) = body {
        line("header", "Content-Type: application/json");
        line("data-raw", body);
    }
    line("write-out", "\n%{http_code}");
    config
}

impl Http {
    pub fn send(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<Reply> {
        let config = curl_config(method, url, headers, body);
        let mut child = curl_command()
            .args(["--silent", "--show-error", "--globoff", "--max-time", "40", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::other(format!("Could not run curl: {e}. gitsot uses the curl command to talk to the Git host.")))?;
        if let Some(mut stdin) = child.stdin.take() {
            // curl may exit before reading everything; its error is reported below.
            let _ = stdin.write_all(config.as_bytes());
        }
        let output = child.wait_with_output().map_err(|e| Error::other(format!("curl failed: {e}")))?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let text = if text.is_empty() { "curl failed without any output".to_string() } else { text };
            return Err(Error::other(format!("Could not reach the Git host.\n\n{text}")));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        let status = status.trim().parse().map_err(|_| Error::other("curl gave no HTTP status"))?;
        Ok(Reply { status, body: body.to_string() })
    }
}

/// Keep URL path segments literal.
fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

pub struct GitHub {
    api: String,
    web: String,
}

#[derive(Deserialize)]
struct GhUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
struct GhRef {
    #[serde(default, rename = "ref")]
    name: String,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct GhPull {
    number: u64,
    #[serde(default)]
    title: String,
    user: Option<GhUser>,
    head: GhRef,
    base: GhRef,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    state: String,
    merged_at: Option<String>,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    html_url: String,
    body: Option<String>,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    #[serde(default)]
    changed_files: usize,
}

#[derive(Deserialize)]
struct GhFile {
    filename: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    patch: Option<String>,
    previous_filename: Option<String>,
}

#[derive(Deserialize)]
struct GhReview {
    user: Option<GhUser>,
    #[serde(default)]
    state: String,
    body: Option<String>,
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct GhComment {
    user: Option<GhUser>,
    body: Option<String>,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct GhError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

impl GhPull {
    fn summary(&self) -> PullSummary {
        // A pull request from a fork shows `owner:branch`.
        let from_fork = self.head.label.split_once(':').map(|(o, _)| o) != self.base.label.split_once(':').map(|(o, _)| o);
        let head = if from_fork && !self.head.label.is_empty() { self.head.label.clone() } else { self.head.name.clone() };
        PullSummary {
            number: self.number,
            title: self.title.clone(),
            author: login(&self.user),
            head,
            base: self.base.name.clone(),
            draft: self.draft,
            state: if self.merged_at.is_some() {
                PullState::Merged
            } else if self.state == "closed" {
                PullState::Closed
            } else {
                PullState::Open
            },
            updated: parse_time(&self.updated_at),
            url: self.html_url.clone(),
        }
    }
}

fn login(user: &Option<GhUser>) -> String {
    user.as_ref().map(|u| u.login.clone()).filter(|l| !l.is_empty()).unwrap_or_else(|| "unknown".into())
}

impl GitHub {
    pub fn new(remote: &Remote) -> Self {
        let repo = format!("{}/{}", encode_segment(&remote.owner), encode_segment(&remote.name));
        let (api, web) = if remote.host == "github.com" {
            (format!("https://api.github.com/repos/{repo}"), format!("https://github.com/{repo}"))
        } else {
            // GitHub Enterprise Server.
            (format!("https://{}/api/v3/repos/{repo}", remote.host), format!("https://{}/{repo}", remote.host))
        };
        GitHub { api, web }
    }

    fn call(&self, http: &Http, method: &str, url: &str, body: Option<&str>) -> Result<String> {
        let mut headers = vec![
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
            ("User-Agent", "gitsot".to_string()),
        ];
        if let Some(token) = &http.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let reply = http.send(method, url, &headers, body)?;
        if (200..300).contains(&reply.status) {
            return Ok(reply.body);
        }
        Err(github_error(reply.status, &reply.body, http.token.is_some()))
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, http: &Http, url: &str) -> Result<T> {
        let body = self.call(http, "GET", url, None)?;
        serde_json::from_str(&body).map_err(|e| Error::other(format!("The host sent an answer gitsot does not understand: {e}")))
    }
}

/// Turn a failed GitHub answer into a message for the user.
fn github_error(status: u16, body: &str, signed_in: bool) -> Error {
    let parsed: Option<GhError> = serde_json::from_str(body).ok();
    let mut detail = parsed.as_ref().map(|e| e.message.clone()).unwrap_or_default();
    for extra in parsed.iter().flat_map(|e| e.errors.iter()) {
        let text = match extra {
            serde_json::Value::String(s) => s.clone(),
            other => other.get("message").and_then(|m| m.as_str()).unwrap_or_default().to_string(),
        };
        if !text.is_empty() {
            detail.push('\n');
            detail.push_str(&text);
        }
    }
    let rate_limited = detail.to_lowercase().contains("rate limit");
    let (kind, hint) = match status {
        401 => (ErrorKind::Auth, "The host rejected the sign-in. The token may have expired."),
        403 if rate_limited && !signed_in => (ErrorKind::Auth, "The host limits requests that are not signed in."),
        403 if rate_limited => (ErrorKind::Other, "The host is limiting requests. Try again in a few minutes."),
        403 if !signed_in => (ErrorKind::Auth, "You need to sign in for this."),
        403 => (ErrorKind::Other, "This account or token is not allowed to do that. A token needs access to pull requests."),
        404 if !signed_in => (ErrorKind::Auth, "Not found. If the repository is private, sign in."),
        404 => (ErrorKind::NotFound, "Not found, or this account has no access to it."),
        422 => (ErrorKind::Other, "The host refused the request."),
        _ => (ErrorKind::Other, "The host answered with an error."),
    };
    let message = if detail.is_empty() { format!("{hint} (HTTP {status})") } else { format!("{hint}\n\n{detail} (HTTP {status})") };
    Error { kind, message }
}

impl Provider for GitHub {
    fn list(&self, http: &Http, filter: Filter) -> Result<Vec<PullSummary>> {
        let state = match filter {
            Filter::Open => "open",
            Filter::Closed => "closed",
            Filter::All => "all",
        };
        let url = format!("{}/pulls?state={state}&sort=updated&direction=desc&per_page={LIST_LIMIT}", self.api);
        let pulls: Vec<GhPull> = self.get(http, &url)?;
        Ok(pulls.iter().map(GhPull::summary).collect())
    }

    fn detail(&self, http: &Http, number: u64) -> Result<PullDetail> {
        let pull: GhPull = self.get(http, &format!("{}/pulls/{number}", self.api))?;

        let mut files = Vec::new();
        for page in 1..=MAX_FILES / 100 {
            let batch: Vec<GhFile> = self.get(http, &format!("{}/pulls/{number}/files?per_page=100&page={page}", self.api))?;
            let last = batch.len() < 100;
            files.extend(batch.into_iter().map(pull_file));
            if last {
                break;
            }
        }

        let reviews: Vec<GhReview> = self.get(http, &format!("{}/pulls/{number}/reviews?per_page=100", self.api))?;
        let comments: Vec<GhComment> = self.get(http, &format!("{}/issues/{number}/comments?per_page=100", self.api))?;
        let mut notes: Vec<Note> = reviews
            .into_iter()
            .map(|r| Note {
                author: login(&r.user),
                review: Some(match r.state.as_str() {
                    "APPROVED" => ReviewState::Approved,
                    "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                    "DISMISSED" => ReviewState::Dismissed,
                    "PENDING" => ReviewState::Pending,
                    _ => ReviewState::Commented,
                }),
                body: r.body.unwrap_or_default(),
                time: parse_time(r.submitted_at.as_deref().unwrap_or_default()),
            })
            // A review with only line comments has no text of its own.
            .filter(|n| n.review != Some(ReviewState::Commented) || !n.body.trim().is_empty())
            .chain(comments.into_iter().map(|c| Note {
                author: login(&c.user),
                review: None,
                body: c.body.unwrap_or_default(),
                time: parse_time(&c.created_at),
            }))
            .collect();
        notes.sort_by_key(|n| n.time);

        let viewer = if http.token.is_some() { self.viewer(http).ok() } else { None };
        Ok(PullDetail {
            summary: pull.summary(),
            body: pull.body.clone().unwrap_or_default(),
            additions: pull.additions,
            deletions: pull.deletions,
            files_truncated: pull.changed_files > files.len(),
            files,
            notes,
            viewer,
        })
    }

    fn review(&self, http: &Http, number: u64, event: ReviewEvent, body: &str) -> Result<()> {
        let event = match event {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        };
        let mut payload = serde_json::Map::new();
        payload.insert("event".into(), event.into());
        if !body.trim().is_empty() {
            payload.insert("body".into(), body.trim().into());
        }
        let payload = serde_json::Value::Object(payload).to_string();
        self.call(http, "POST", &format!("{}/pulls/{number}/reviews", self.api), Some(&payload))?;
        Ok(())
    }

    fn viewer(&self, http: &Http) -> Result<String> {
        // `…/repos/owner/name` -> `…/user`
        let root = self.api.rsplitn(4, '/').nth(3).unwrap_or_default();
        let user: GhUser = self.get(http, &format!("{root}/user"))?;
        Ok(user.login)
    }

    fn web_url(&self) -> String {
        format!("{}/pulls", self.web)
    }
}

fn pull_file(file: GhFile) -> PullFile {
    let kind = match file.status.as_str() {
        "added" | "copied" => ChangeKind::Added,
        "removed" => ChangeKind::Deleted,
        "renamed" => ChangeKind::Renamed,
        _ => ChangeKind::Modified,
    };
    let mut diff = match &file.patch {
        Some(patch) => parse_patch(patch),
        None => FileDiff::default(),
    };
    if let Some(old) = &file.previous_filename {
        diff.lines.insert(0, info_line(format!("Renamed from {old}")));
    }
    if file.patch.is_none() && (file.additions > 0 || file.deletions > 0 || file.previous_filename.is_none()) {
        diff.lines.push(info_line("The host sent no text diff for this file (binary, or too large).".into()));
    }
    PullFile { path: file.filename, kind, additions: file.additions, deletions: file.deletions, diff }
}

fn info_line(text: String) -> DiffLine {
    DiffLine { kind: LineKind::Info, old_no: None, new_no: None, text }
}

/// Turn the hunks of a unified diff (`@@ -1,3 +1,4 @@` and the lines after
/// it) into the lines the diff view draws.
pub fn parse_patch(patch: &str) -> FileDiff {
    let mut out = FileDiff::default();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in patch.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("@@") {
            if let Some((old, new)) = hunk_start(line) {
                old_no = old;
                new_no = new;
            }
            out.lines.push(DiffLine { kind: LineKind::Hunk, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            // "\ No newline at end of file"
            Some('\\') => (LineKind::Info, line[1..].trim()),
            // An empty context line that lost its leading space.
            None => (LineKind::Context, ""),
            Some(_) => (LineKind::Info, line),
        };
        let (old, new) = match kind {
            LineKind::Added => {
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                old_no += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            _ => (None, None),
        };
        out.lines.push(DiffLine { kind, old_no: old, new_no: new, text: text.to_string() });
    }
    out
}

/// `@@ -12,7 +14,8 @@ fn x()` -> `(12, 14)`
fn hunk_start(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?.split(',').next()?.parse().ok()?;
    let new = parts.next()?.strip_prefix('+')?.split(',').next()?.parse().ok()?;
    Some((old, new))
}

/// `2026-11-18T06:56:03Z` -> seconds since the Unix epoch (0 if unreadable).
pub fn parse_time(text: &str) -> i64 {
    let digits = |range: std::ops::Range<usize>| text.get(range).and_then(|s| s.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d), Some(h), Some(min), Some(s)) =
        (digits(0..4), digits(5..7), digits(8..10), digits(11..13), digits(14..16), digits(17..19))
    else {
        return 0;
    };
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + h * 3600 + min * 60 + s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(url: &str) -> Remote {
        Remote::parse(url).unwrap_or_else(|| panic!("{url} should parse"))
    }

    #[test]
    fn remotes_of_every_shape() {
        let r = remote("https://github.com/profawie/gitsot.git");
        assert_eq!((r.kind, r.host.as_str(), r.owner.as_str(), r.name.as_str(), r.http), (Kind::GitHub, "github.com", "profawie", "gitsot", true));
        let r = remote("git@github.com:profawie/gitsot.git");
        assert_eq!((r.kind, r.owner.as_str(), r.name.as_str(), r.http), (Kind::GitHub, "profawie", "gitsot", false));
        let r = remote("ssh://git@gitlab.example.com:2222/group/sub/project.git");
        assert_eq!((r.kind, r.host.as_str(), r.owner.as_str(), r.name.as_str()), (Kind::GitLab, "gitlab.example.com", "group/sub", "project"));
        let r = remote("https://me@bitbucket.org/team/repo");
        assert_eq!((r.kind, r.owner.as_str(), r.name.as_str()), (Kind::Bitbucket, "team", "repo"));
        assert_eq!(remote("https://codeberg.org/a/b.git").kind, Kind::Gitea);
        assert_eq!(remote("https://git.company.example/a/b.git").kind, Kind::Unknown);
    }

    #[test]
    fn local_remotes_are_not_hosts() {
        for url in ["/srv/git/project.git", "../project", r"C:\work\project", "file:///srv/git/project.git", "project"] {
            assert_eq!(Remote::parse(url), None, "{url}");
        }
    }

    #[test]
    fn only_github_has_a_provider_for_now() {
        assert!(remote("https://github.com/a/b").provider().is_some());
        assert!(remote("https://gitlab.com/a/b").provider().is_none());
    }

    #[test]
    fn github_urls() {
        let hub = GitHub::new(&remote("git@github.com:profawie/gitsot.git"));
        assert_eq!(hub.api, "https://api.github.com/repos/profawie/gitsot");
        assert_eq!(hub.web_url(), "https://github.com/profawie/gitsot/pulls");
        let mut enterprise = remote("https://git.company.example/team/app.git");
        enterprise.kind = Kind::GitHub;
        assert_eq!(GitHub::new(&enterprise).api, "https://git.company.example/api/v3/repos/team/app");
        assert_eq!(encode_segment("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn curl_config_keeps_secrets_and_bodies_intact() {
        let body = r#"{"body":"line \"one\"\nline two \\ done"}"#;
        let config = curl_config("POST", "https://x/y", &[("Authorization", "Bearer abc\r\nX-Evil: 1".into())], Some(body));
        assert!(config.contains("url = \"https://x/y\"\n"));
        assert!(config.contains("header = \"Authorization: Bearer abcX-Evil: 1\"\n"));
        assert!(config.contains(r#"data-raw = "{\"body\":\"line \\\"one\\\"\\nline two \\\\ done\"}""#));
        assert!(config.contains("proto = \"=https\"\n"));
    }

    #[test]
    fn patches_become_numbered_lines() {
        let diff = parse_patch("@@ -10,3 +10,4 @@ fn main() {\n context\n-old\n+new\n+more\n\\ No newline at end of file");
        let rows: Vec<_> = diff.lines.iter().map(|l| (l.kind, l.old_no, l.new_no, l.text.as_str())).collect();
        assert_eq!(
            rows,
            vec![
                (LineKind::Hunk, None, None, "@@ -10,3 +10,4 @@ fn main() {"),
                (LineKind::Context, Some(10), Some(10), "context"),
                (LineKind::Removed, Some(11), None, "old"),
                (LineKind::Added, None, Some(11), "new"),
                (LineKind::Added, None, Some(12), "more"),
                (LineKind::Info, None, None, "No newline at end of file"),
            ]
        );
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_time("2026-11-18T06:56:03Z"), 1_794_984_963);
        assert_eq!(parse_time("2024-02-29T12:00:00Z"), 1_709_208_000);
        assert_eq!(parse_time("soon"), 0);
    }

    #[test]
    fn github_answers() {
        let pull: GhPull = serde_json::from_str(
            r#"{"number":7,"title":"Add tabs","user":{"login":"ana"},"state":"closed","merged_at":"2026-01-02T03:04:05Z",
                "updated_at":"2026-01-02T03:04:05Z","html_url":"https://github.com/a/b/pull/7","draft":false,
                "head":{"ref":"tabs","label":"fork:tabs"},"base":{"ref":"main","label":"a:main"},"body":null}"#,
        )
        .unwrap();
        let s = pull.summary();
        assert_eq!((s.number, s.author.as_str(), s.head.as_str(), s.base.as_str(), s.state), (7, "ana", "fork:tabs", "main", PullState::Merged));

        let file: GhFile = serde_json::from_str(r#"{"filename":"logo.png","status":"added","additions":0,"deletions":0}"#).unwrap();
        let file = pull_file(file);
        assert_eq!(file.kind, ChangeKind::Added);
        assert_eq!(file.diff.lines.len(), 1);

        let e = github_error(404, r#"{"message":"Not Found"}"#, false);
        assert_eq!(e.kind, ErrorKind::Auth);
        let e = github_error(422, r#"{"message":"Unprocessable Entity","errors":["Can not approve your own pull request"]}"#, true);
        assert_eq!(e.kind, ErrorKind::Other);
        assert!(e.message.contains("Can not approve your own pull request"));
    }
}
