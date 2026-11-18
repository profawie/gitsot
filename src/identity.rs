//! Git identities (user.name / user.email): global, per-repository, and
//! per-folder rules.
//!
//! Folder rules are stored the way Git itself understands them, so the `git`
//! command and other tools follow them too:
//!
//! ```text
//! # ~/.gitconfig
//! [includeIf "gitdir:C:/work/company/"]
//!     path = C:/Users/me/.config/gitsot/identities/company.gitconfig
//! ```
//!
//! and the included file holds `[user] name = … email = …`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use git2::{ConfigLevel, Repository};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    pub fn is_set(&self) -> bool {
        !self.email.trim().is_empty()
    }

    pub fn display(&self) -> String {
        match (self.name.trim().is_empty(), self.email.trim().is_empty()) {
            (true, true) => "not set".into(),
            (false, true) => self.name.clone(),
            (true, false) => format!("<{}>", self.email),
            (false, false) => format!("{} <{}>", self.name, self.email),
        }
    }

    fn from_config(cfg: &git2::Config) -> Self {
        Identity {
            name: cfg.get_string("user.name").unwrap_or_default(),
            email: cfg.get_string("user.email").unwrap_or_default(),
        }
    }
}

/// A "use this identity for every repository under this folder" rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRule {
    /// Folder as written in the rule, with forward slashes and a trailing `/`.
    pub folder: String,
    pub identity: Identity,
    /// The include file Gitsot wrote for this rule.
    pub file: PathBuf,
    /// Full config key of the `includeIf … path` entry, used to remove it.
    key: String,
}

/// Where a repository's identity comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Local,
    Rule(String),
    Global,
    Missing,
}

/// The identity check for one repository.
#[derive(Clone, Debug)]
pub struct Status {
    /// What git will actually use for commits in this repository.
    pub effective: Identity,
    pub source: Source,
    /// The folder rule covering this repository, if any.
    pub rule: Option<FolderRule>,
}

impl Status {
    /// The repository is covered by a folder rule but commits would use a different email.
    pub fn mismatch(&self) -> Option<&FolderRule> {
        self.rule
            .as_ref()
            .filter(|r| !r.identity.email.trim().eq_ignore_ascii_case(self.effective.email.trim()))
    }
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Could not find your home folder"))
}

/// The user's global git config file, created if it does not exist yet.
pub fn global_config_path() -> Result<PathBuf> {
    if let Ok(p) = git2::Config::find_global() {
        return Ok(p);
    }
    let p = home()?.join(".gitconfig");
    if !p.exists() {
        std::fs::write(&p, "")?;
    }
    Ok(p)
}

/// Folder where Gitsot keeps the per-rule include files.
pub fn identities_dir() -> Result<PathBuf> {
    Ok(home()?.join(".config").join("gitsot").join("identities"))
}

pub fn global() -> Identity {
    match global_config_path().and_then(|p| Ok(git2::Config::open(&p)?)) {
        Ok(cfg) => Identity::from_config(&cfg),
        Err(_) => Identity::default(),
    }
}

pub fn set_global(id: &Identity) -> Result<()> {
    let mut cfg = git2::Config::open(&global_config_path()?)?;
    cfg.set_str("user.name", id.name.trim())?;
    cfg.set_str("user.email", id.email.trim())?;
    Ok(())
}

/// Identity set in this repository's own `.git/config`, if any.
pub fn local(repo: &Repository) -> Option<Identity> {
    let cfg = repo.config().ok()?.open_level(ConfigLevel::Local).ok()?;
    let id = Identity::from_config(&cfg);
    (id.is_set() || !id.name.is_empty()).then_some(id)
}

pub fn set_local(repo: &Repository, id: &Identity) -> Result<()> {
    if id.email.trim().is_empty() {
        bail!("Please enter an email");
    }
    let mut cfg = repo.config()?.open_level(ConfigLevel::Local)?;
    cfg.set_str("user.name", id.name.trim())?;
    cfg.set_str("user.email", id.email.trim())?;
    Ok(())
}

pub fn clear_local(repo: &Repository) -> Result<()> {
    let mut cfg = repo.config()?.open_level(ConfigLevel::Local)?;
    let _ = cfg.remove("user.name");
    let _ = cfg.remove("user.email");
    Ok(())
}

/// What git will use in `repo` (all config levels, including folder rules).
pub fn effective(repo: &Repository) -> Identity {
    repo.config().map(|c| Identity::from_config(&c)).unwrap_or_default()
}

fn normalize_folder(folder: &str) -> String {
    let mut f = folder.trim().replace('\\', "/");
    // `canonicalize` on Windows gives \\?\C:\…
    if let Some(rest) = f.strip_prefix("//?/") {
        f = rest.to_string();
    }
    if !f.ends_with('/') {
        f.push('/');
    }
    f
}

fn path_key(p: &Path) -> String {
    let s = normalize_folder(&p.to_string_lossy());
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Does `folder` (a rule folder) contain `repo_path`?
pub fn folder_contains(folder: &str, repo_path: &Path) -> bool {
    let f = path_key(Path::new(folder));
    path_key(repo_path).starts_with(&f)
}

/// The folder rules Gitsot manages, read from the global git config.
pub fn rules() -> Vec<FolderRule> {
    rules_in(&match global_config_path() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    })
}

fn rules_in(global: &Path) -> Vec<FolderRule> {
    let Ok(cfg) = git2::Config::open(global) else { return Vec::new() };
    let Ok(dir) = identities_dir() else { return Vec::new() };
    let dir_key = path_key(&dir);
    let mut out = Vec::new();
    let Ok(mut entries) = cfg.entries(Some(r"includeif\..*\.path")) else { return out };
    while let Some(Ok(entry)) = entries.next() {
        let (Ok(key), Ok(value)) = (entry.name().map(str::to_string), entry.value().map(str::to_string)) else {
            continue;
        };
        let file = PathBuf::from(&value);
        // Only rules Gitsot created (others are left alone and not shown).
        if !path_key(&file).starts_with(&dir_key) {
            continue;
        }
        let Some(cond) = key.strip_prefix("includeif.").and_then(|k| k.strip_suffix(".path")) else {
            continue;
        };
        let Some((_, folder)) = cond.split_once(':') else { continue };
        let identity = git2::Config::open(&file).map(|c| Identity::from_config(&c)).unwrap_or_default();
        out.push(FolderRule { folder: folder.to_string(), identity, file, key });
    }
    out.sort_by(|a, b| a.folder.cmp(&b.folder));
    out
}

pub fn add_rule(folder: &Path, id: &Identity) -> Result<()> {
    add_rule_in(&global_config_path()?, &identities_dir()?, folder, id)
}

fn add_rule_in(global: &Path, dir: &Path, folder: &Path, id: &Identity) -> Result<()> {
    if id.email.trim().is_empty() {
        bail!("Please enter an email for this folder");
    }
    let folder_s = folder.to_string_lossy();
    if folder_s.trim().is_empty() {
        bail!("Please choose a folder");
    }
    let folder = normalize_folder(&folder_s);
    std::fs::create_dir_all(dir).with_context(|| format!("Could not create {}", dir.display()))?;

    // A readable, unique file name from the folder.
    let slug: String = folder
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("folder")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let mut file = dir.join(format!("{slug}.gitconfig"));
    let mut n = 2;
    while file.exists() {
        file = dir.join(format!("{slug}-{n}.gitconfig"));
        n += 1;
    }
    std::fs::write(&file, "")?;
    let mut inc = git2::Config::open(&file)?;
    inc.set_str("user.name", id.name.trim())?;
    inc.set_str("user.email", id.email.trim())?;

    let cond = if cfg!(windows) { "gitdir/i" } else { "gitdir" };
    let mut cfg = git2::Config::open(global)?;
    // Replace an existing rule for the same folder.
    for r in rules_in(global) {
        if path_key(Path::new(&r.folder)) == path_key(Path::new(&folder)) {
            remove_rule_in(global, &r)?;
        }
    }
    let file_s = file.to_string_lossy().replace('\\', "/");
    cfg.set_str(&format!("includeIf.{cond}:{folder}.path"), &file_s)?;
    Ok(())
}

pub fn remove_rule(rule: &FolderRule) -> Result<()> {
    remove_rule_in(&global_config_path()?, rule)
}

fn remove_rule_in(global: &Path, rule: &FolderRule) -> Result<()> {
    let mut cfg = git2::Config::open(global)?;
    cfg.remove(&rule.key)?;
    let _ = std::fs::remove_file(&rule.file);
    Ok(())
}

/// The longest (most specific) rule covering `repo_path`.
pub fn rule_for<'a>(rules: &'a [FolderRule], repo_path: &Path) -> Option<&'a FolderRule> {
    rules
        .iter()
        .filter(|r| folder_contains(&r.folder, repo_path))
        .max_by_key(|r| r.folder.len())
}

pub fn status(repo: &Repository, repo_path: &Path, rules: &[FolderRule]) -> Status {
    let effective = effective(repo);
    let rule = rule_for(rules, repo_path).cloned();
    let source = if local(repo).is_some() {
        Source::Local
    } else if let Some(r) = &rule {
        if r.identity.email.eq_ignore_ascii_case(&effective.email) {
            Source::Rule(r.folder.clone())
        } else {
            Source::Global
        }
    } else if effective.is_set() {
        Source::Global
    } else {
        Source::Missing
    };
    Status { effective, source, rule }
}

/// Author emails of commits that `git push` would send (HEAD minus anything any
/// remote already has), as (short id, email) pairs.
pub fn unpushed_authors(repo: &Repository) -> Vec<(String, String)> {
    let Ok(mut walk) = repo.revwalk() else { return Vec::new() };
    if walk.push_head().is_err() {
        return Vec::new();
    }
    let _ = walk.hide_glob("refs/remotes/*");
    walk.flatten()
        .take(500)
        .filter_map(|oid| repo.find_commit(oid).ok())
        .map(|c| (crate::git::short(c.id()), c.author().email().unwrap_or("").to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_matching() {
        assert!(folder_contains("/work/company/", Path::new("/work/company/app")));
        assert!(folder_contains("/work/company", Path::new("/work/company/app/")));
        assert!(!folder_contains("/work/company/", Path::new("/work/company-old/app")));
        assert!(!folder_contains("/work/company/", Path::new("/home/me/app")));
        assert_eq!(normalize_folder(r"C:\work\company"), "C:/work/company/");
    }

    #[test]
    fn rules_round_trip_and_mismatch() {
        let tmp = std::env::temp_dir().join(format!("gitsot-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let global = tmp.join("gitconfig");
        std::fs::write(&global, "[user]\n\tname = Me\n\temail = me@personal.dev\n").unwrap();
        let dir = identities_dir().unwrap();

        let company = tmp.join("company");
        let id = Identity { name: "Me".into(), email: "me@company.com".into() };
        add_rule_in(&global, &dir, &company, &id).unwrap();
        let rules = rules_in(&global);
        let ours: Vec<_> = rules.iter().filter(|r| r.folder.contains("gitsot-id-")).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].identity, id);
        assert!(rule_for(&rules, &company.join("app")).is_some());
        assert!(rule_for(&rules, &tmp.join("personal").join("app")).is_none());

        // A repository under the company folder with a personal local identity is a mismatch.
        let repo_path = company.join("app");
        let repo = Repository::init(&repo_path).unwrap();
        set_local(&repo, &Identity { name: "Me".into(), email: "me@personal.dev".into() }).unwrap();
        let st = status(&repo, &repo_path, &rules);
        assert_eq!(st.source, Source::Local);
        assert!(st.mismatch().is_some());
        set_local(&repo, &id).unwrap();
        assert!(status(&repo, &repo_path, &rules).mismatch().is_none());
        clear_local(&repo).unwrap();
        assert!(local(&repo).is_none());

        // Replacing and removing.
        add_rule_in(&global, &dir, &company, &Identity { name: "X".into(), email: "x@company.com".into() }).unwrap();
        let ours: Vec<_> = rules_in(&global).into_iter().filter(|r| r.folder.contains("gitsot-id-")).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].identity.email, "x@company.com");
        remove_rule_in(&global, &ours[0]).unwrap();
        assert!(rules_in(&global).iter().all(|r| !r.folder.contains("gitsot-id-")));
        assert!(!ours[0].file.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod include_if_tests {
    use super::*;

    /// The rule must be written in a form `git` itself applies.
    #[test]
    fn git_cli_applies_folder_rule() {
        let tmp = std::env::temp_dir().join(format!("gitsot-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let global = home.join(".gitconfig");
        std::fs::write(&global, "[user]\n\tname = Me\n\temail = me@personal.dev\n").unwrap();
        let company = tmp.join("company");
        let id = Identity { name: "Me Work".into(), email: "me@company.com".into() };
        add_rule_in(&global, &tmp.join("ids"), &company, &id).unwrap();

        let repo_path = company.join("app");
        Repository::init(&repo_path).unwrap();
        // Use the git CLI as the reference implementation when available.
        let out = std::process::Command::new("git")
            .args(["config", "user.email"])
            .current_dir(&repo_path)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output();
        if let Ok(out) = out {
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "me@company.com");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
