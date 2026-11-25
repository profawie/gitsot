//! Thin, synchronous helpers around `git2` for everything that happens locally:
//! status, diffs, staging, committing, branches and history.
//!
//! Network operations (clone / fetch / pull / push) live in `cli.rs`, because the
//! `git` command line already knows how to talk to credential managers, SSH agents
//! and proxies on every platform.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use git2::{
    BranchType, Delta, DiffFormat, DiffOptions, IndexAddOption, ObjectType, Oid, Repository,
    Signature, Sort, Status, StatusOptions,
};

/// Maximum number of diff lines rendered for a single file.
pub const MAX_DIFF_LINES: usize = 20_000;
/// Maximum number of commits loaded in the history list.
pub const MAX_HISTORY: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    pub fn letter(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::TypeChange => "T",
            ChangeKind::Untracked => "U",
            ChangeKind::Conflicted => "!",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ChangeKind::Added => "Added",
            ChangeKind::Modified => "Modified",
            ChangeKind::Deleted => "Deleted",
            ChangeKind::Renamed => "Renamed",
            ChangeKind::TypeChange => "Type changed",
            ChangeKind::Untracked => "New file",
            ChangeKind::Conflicted => "Conflicted",
        }
    }

    fn from_delta(delta: Delta) -> Self {
        match delta {
            Delta::Added | Delta::Copied => ChangeKind::Added,
            Delta::Deleted => ChangeKind::Deleted,
            Delta::Renamed => ChangeKind::Renamed,
            Delta::Typechange => ChangeKind::TypeChange,
            Delta::Untracked => ChangeKind::Untracked,
            Delta::Conflicted => ChangeKind::Conflicted,
            _ => ChangeKind::Modified,
        }
    }
}

/// One changed file in the working directory.
#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    /// Has changes recorded in the index (staged).
    pub staged: bool,
    /// Has changes in the working tree that are not staged.
    pub unstaged: bool,
}

impl FileChange {
    pub fn fully_staged(&self) -> bool {
        self.staged && !self.unstaged
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Hunk,
    Info,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
    pub binary: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    pub message: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub parents: Vec<Oid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

/// A branch / remote branch / tag pointing at a commit, shown as a label in history.
#[derive(Clone, Debug)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
    pub is_head: bool,
}

#[derive(Clone, Debug)]
pub struct CommitFile {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub upstream: Option<String>,
    /// Commits this branch has that its upstream does not.
    pub ahead: usize,
    /// Commits the upstream has that this branch does not.
    pub behind: usize,
}

#[derive(Clone, Debug, Default)]
pub struct HeadInfo {
    /// Branch name, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Short id of the HEAD commit (empty for a brand-new repository).
    pub short_id: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub has_remote: bool,
    /// Human readable description of an in-progress operation (merge, rebase, ...).
    pub state: Option<String>,
}

impl HeadInfo {
    pub fn display_name(&self) -> String {
        match &self.branch {
            Some(b) => b.clone(),
            None if self.short_id.is_empty() => "(no branch)".to_string(),
            None => format!("detached at {}", self.short_id),
        }
    }
}

pub fn open(path: &Path) -> Result<Repository> {
    let repo = Repository::open(path)
        .with_context(|| format!("'{}' is not a Git repository", path.display()))?;
    if repo.is_bare() {
        bail!("Bare repositories are not supported");
    }
    Ok(repo)
}

pub fn init(path: &Path) -> Result<Repository> {
    std::fs::create_dir_all(path)?;
    let repo = Repository::init(path)?;
    Ok(repo)
}

pub fn workdir(repo: &Repository) -> PathBuf {
    repo.workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo.path().to_path_buf())
}

pub fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------
// Status / HEAD
// ---------------------------------------------------------------------------

pub fn status(repo: &Repository) -> Result<Vec<FileChange>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(false)
        .renames_index_to_workdir(false);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut out = Vec::with_capacity(statuses.len());
    for entry in statuses.iter() {
        let s = entry.status();
        if s.is_empty() || s.contains(Status::IGNORED) {
            continue;
        }
        let path = entry
            .index_to_workdir()
            .and_then(|d| d.new_file().path())
            .or_else(|| entry.head_to_index().and_then(|d| d.new_file().path()))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .or_else(|| entry.path().ok().map(str::to_string))
            .unwrap_or_default();

        let staged = s.intersects(
            Status::INDEX_NEW
                | Status::INDEX_MODIFIED
                | Status::INDEX_DELETED
                | Status::INDEX_RENAMED
                | Status::INDEX_TYPECHANGE,
        );
        let unstaged = s.intersects(
            Status::WT_NEW
                | Status::WT_MODIFIED
                | Status::WT_DELETED
                | Status::WT_RENAMED
                | Status::WT_TYPECHANGE,
        );

        let kind = if s.contains(Status::CONFLICTED) {
            ChangeKind::Conflicted
        } else if s.contains(Status::WT_NEW) {
            ChangeKind::Untracked
        } else if s.intersects(Status::INDEX_NEW) {
            ChangeKind::Added
        } else if s.intersects(Status::INDEX_DELETED | Status::WT_DELETED) {
            ChangeKind::Deleted
        } else if s.intersects(Status::INDEX_RENAMED | Status::WT_RENAMED) {
            ChangeKind::Renamed
        } else if s.intersects(Status::INDEX_TYPECHANGE | Status::WT_TYPECHANGE) {
            ChangeKind::TypeChange
        } else {
            ChangeKind::Modified
        };

        out.push(FileChange {
            path,
            kind,
            staged,
            unstaged: unstaged || kind == ChangeKind::Conflicted,
        });
    }
    out.sort_by_key(|c| c.path.to_lowercase());
    Ok(out)
}

pub fn head_info(repo: &Repository) -> HeadInfo {
    let mut info = HeadInfo {
        has_remote: repo.remotes().map(|r| !r.is_empty()).unwrap_or(false),
        ..Default::default()
    };

    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                info.branch = head.shorthand().ok().map(str::to_string);
            }
            if let Some(oid) = head.target() {
                info.short_id = short(oid);
            }
            if let Some(name) = &info.branch {
                if let Ok(branch) = repo.find_branch(name, BranchType::Local) {
                    if let Ok(up) = branch.upstream() {
                        info.upstream = up.name().ok().flatten().map(str::to_string);
                        if let (Some(local), Some(remote)) = (branch.get().target(), up.get().target())
                        {
                            if let Ok((a, b)) = repo.graph_ahead_behind(local, remote) {
                                info.ahead = a;
                                info.behind = b;
                            }
                        }
                    }
                }
            }
        }
        Err(_) => {
            // Unborn branch (fresh repository): read the symbolic target of HEAD.
            if let Ok(head) = repo.find_reference("HEAD") {
                if let Ok(Some(target)) = head.symbolic_target() {
                    info.branch = Some(target.trim_start_matches("refs/heads/").to_string());
                }
            }
        }
    }

    info.state = match repo.state() {
        git2::RepositoryState::Clean => None,
        git2::RepositoryState::Merge => Some("Merge in progress".into()),
        git2::RepositoryState::Revert | git2::RepositoryState::RevertSequence => {
            Some("Revert in progress".into())
        }
        git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
            Some("Cherry-pick in progress".into())
        }
        git2::RepositoryState::Bisect => Some("Bisect in progress".into()),
        _ => Some("Rebase in progress".into()),
    };
    info
}

pub fn short(oid: Oid) -> String {
    let s = oid.to_string();
    s[..7.min(s.len())].to_string()
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

pub fn stage(repo: &Repository, paths: &[String]) -> Result<()> {
    let mut index = repo.index()?;
    let root = workdir(repo);
    for p in paths {
        if root.join(p).symlink_metadata().is_ok() {
            index.add_path(Path::new(p))?;
        } else {
            // File was deleted in the working tree: stage the deletion.
            index.remove_path(Path::new(p))?;
        }
    }
    index.write()?;
    Ok(())
}

pub fn stage_all(repo: &Repository) -> Result<()> {
    let mut index = repo.index()?;
    index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
    index.update_all(["*"].iter(), None)?;
    index.write()?;
    Ok(())
}

pub fn unstage(repo: &Repository, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(commit) => {
            repo.reset_default(Some(commit.as_object()), paths.iter())?;
        }
        None => {
            // No commits yet: unstaging means removing from the index.
            let mut index = repo.index()?;
            for p in paths {
                let _ = index.remove_path(Path::new(p));
            }
            index.write()?;
        }
    }
    Ok(())
}

pub fn unstage_all(repo: &Repository) -> Result<()> {
    let paths: Vec<String> = status(repo)?
        .into_iter()
        .filter(|c| c.staged)
        .map(|c| c.path)
        .collect();
    unstage(repo, &paths)
}

/// Throw away all local changes to the given files (staged and unstaged).
pub fn discard(repo: &Repository, changes: &[FileChange]) -> Result<()> {
    let root = workdir(repo);
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut restore = Vec::new();
    let mut index = repo.index()?;

    for c in changes {
        let in_head = head_tree
            .as_ref()
            .map(|t| t.get_path(Path::new(&c.path)).is_ok())
            .unwrap_or(false);
        if in_head {
            restore.push(c.path.clone());
        } else {
            // File does not exist in HEAD: remove it from index and disk.
            let _ = index.remove_path(Path::new(&c.path));
            let full = root.join(&c.path);
            if full.is_dir() {
                std::fs::remove_dir_all(&full)
                    .with_context(|| format!("Could not delete {}", full.display()))?;
            } else if full.symlink_metadata().is_ok() {
                std::fs::remove_file(&full)
                    .with_context(|| format!("Could not delete {}", full.display()))?;
            }
        }
    }
    index.write()?;

    if !restore.is_empty() {
        if let Some(commit) = repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            repo.reset_default(Some(commit.as_object()), restore.iter())?;
        }
        let mut cb = git2::build::CheckoutBuilder::new();
        cb.force();
        for p in &restore {
            cb.path(p);
        }
        repo.checkout_head(Some(&mut cb))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

pub fn signature(repo: &Repository) -> Result<Signature<'static>> {
    repo.signature()
        .map(|s| s.to_owned())
        .map_err(|_| anyhow!("Your Git name and email are not set"))
}

pub fn commit(repo: &Repository, message: &str) -> Result<Oid> {
    let sig = signature(repo)?;
    let mut index = repo.index()?;
    // The git command (merge, cherry-pick) may have changed it on disk.
    index.read(false)?;
    if index.has_conflicts() {
        bail!("Resolve all conflicts before committing");
    }
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let mut parents = Vec::new();
    if let Ok(head) = repo.head() {
        parents.push(head.peel_to_commit()?);
    }
    let merging = repo.state() == git2::RepositoryState::Merge;
    if merging {
        let merge_head = std::fs::read_to_string(repo.path().join("MERGE_HEAD")).unwrap_or_default();
        for id in merge_head.lines().filter_map(|l| Oid::from_str(l.trim()).ok()) {
            parents.push(repo.find_commit(id)?);
        }
    }
    // Finishing a cherry-pick that had conflicts: the picked commit keeps its
    // author, and the person finishing it is the committer, like git does.
    let picked = match repo.state() {
        git2::RepositoryState::CherryPick => cherry_pick_head(repo).and_then(|id| repo.find_commit(id).ok()),
        _ => None,
    };

    if !merging {
        if let Some(p) = parents.first() {
            if p.tree_id() == tree_id {
                bail!("There are no staged changes to commit");
            }
        }
    }

    let author = match &picked {
        Some(c) => c.author().to_owned(),
        None => sig.clone(),
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    let oid = repo.commit(Some("HEAD"), &author, &sig, message, &tree, &parent_refs)?;
    if merging || picked.is_some() {
        repo.cleanup_state()?;
    }
    Ok(oid)
}

// ---------------------------------------------------------------------------
// Cherry-pick
// ---------------------------------------------------------------------------

/// The commit being cherry-picked, while a cherry-pick is stopped on conflicts.
pub fn cherry_pick_head(repo: &Repository) -> Option<Oid> {
    let text = std::fs::read_to_string(repo.path().join("CHERRY_PICK_HEAD")).ok()?;
    Oid::from_str(text.trim()).ok()
}

/// What cherry-picking a commit onto the current branch would do, worked out
/// without touching the working folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CherryPickPreview {
    /// Why the commit cannot be cherry-picked right now, in plain words.
    pub blocker: Option<String>,
    /// Files the commit changes that Git cannot apply here by itself.
    pub conflicts: usize,
    /// The commit's changes are already here, so it would add nothing.
    pub empty: bool,
}

/// Preview copying commit `id` onto the current branch.
///
/// A cherry-pick is refused while there are uncommitted changes to tracked
/// files: a conflict would mix them with the picked changes, and aborting
/// could then lose them.
pub fn cherry_pick_preview(repo: &Repository, id: Oid) -> Result<CherryPickPreview> {
    let blocked = |why: String| Ok(CherryPickPreview { blocker: Some(why), ..Default::default() });
    let commit = repo.find_commit(id)?;
    if let Some(state) = head_info(repo).state {
        return blocked(format!("{state}. Finish or abort it first."));
    }
    let Some(head) = repo.head().ok().and_then(|h| h.peel_to_commit().ok()) else {
        return blocked("The current branch has no commits yet.".into());
    };
    if commit.parent_count() > 1 {
        return blocked("This is a merge commit. Cherry-pick the commits it brought in instead.".into());
    }
    if on_head(repo, id) {
        return blocked("This commit is already part of the current branch.".into());
    }
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false).include_ignored(false).exclude_submodules(true);
    let dirty = repo.statuses(Some(&mut opts))?.len();
    if dirty > 0 {
        let files = if dirty == 1 { "1 file has".to_string() } else { format!("{dirty} files have") };
        return blocked(format!("{files} uncommitted changes. Commit or discard them first, then cherry-pick."));
    }
    let mut index = repo.cherrypick_commit(&commit, &head, 0, None)?;
    let mut paths = std::collections::BTreeSet::new();
    for c in index.conflicts()?.flatten() {
        if let Some(entry) = c.our.or(c.their).or(c.ancestor) {
            paths.insert(entry.path);
        }
    }
    let empty = paths.is_empty() && index.write_tree_to(repo)? == head.tree_id();
    Ok(CherryPickPreview { blocker: None, conflicts: paths.len(), empty })
}

// ---------------------------------------------------------------------------
// Diffs
// ---------------------------------------------------------------------------

fn collect_diff(diff: &git2::Diff) -> Result<FileDiff> {
    let mut out = FileDiff::default();
    for delta in diff.deltas() {
        if delta.flags().is_binary() {
            out.binary = true;
        }
    }
    diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        if delta.flags().is_binary() {
            out.binary = true;
        }
        if out.lines.len() >= MAX_DIFF_LINES {
            out.truncated = true;
            return false;
        }
        let text = String::from_utf8_lossy(line.content())
            .trim_end_matches(['\n', '\r'])
            .to_string();
        let kind = match line.origin() {
            '+' => LineKind::Added,
            '-' => LineKind::Removed,
            ' ' => LineKind::Context,
            'H' => LineKind::Hunk,
            '=' | '>' | '<' => LineKind::Info,
            _ => return true, // file headers
        };
        let text = match line.origin() {
            '=' | '>' | '<' => text.trim().to_string(),
            _ => text,
        };
        out.lines.push(DiffLine {
            kind,
            old_no: line.old_lineno(),
            new_no: line.new_lineno(),
            text,
        });
        true
    })
    .or_else(|e| {
        // print() returns a "user" error when the callback stops early.
        if out.truncated {
            Ok(())
        } else {
            Err(e)
        }
    })?;
    Ok(out)
}

fn diff_opts(path: &str) -> DiffOptions {
    let mut opts = DiffOptions::new();
    opts.pathspec(path)
        .disable_pathspec_match(true)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    opts
}

/// Diff of a file in the working tree compared with HEAD (staged + unstaged).
pub fn working_diff(repo: &Repository, path: &str) -> Result<FileDiff> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = diff_opts(path);
    let diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    collect_diff(&diff)
}

fn commit_diff<'r>(repo: &'r Repository, id: Oid, path: Option<&str>) -> Result<git2::Diff<'r>> {
    let commit = repo.find_commit(id)?;
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(p) => Some(p.tree()?),
        Err(_) => None,
    };
    let mut opts = match path {
        Some(p) => diff_opts(p),
        None => DiffOptions::new(),
    };
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
    if path.is_none() {
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;
    }
    Ok(diff)
}

pub fn commit_files(repo: &Repository, id: Oid) -> Result<Vec<CommitFile>> {
    let diff = commit_diff(repo, id, None)?;
    let files = diff
        .deltas()
        .map(|d| CommitFile {
            path: d
                .new_file()
                .path()
                .or_else(|| d.old_file().path())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default(),
            kind: ChangeKind::from_delta(d.status()),
        })
        .collect();
    Ok(files)
}

pub fn commit_file_diff(repo: &Repository, id: Oid, path: &str) -> Result<FileDiff> {
    let diff = commit_diff(repo, id, Some(path))?;
    collect_diff(&diff)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Commits reachable from HEAD, or from every branch and tag when `all_refs` is set
/// (used by the tree view so other branches show up in the graph).
pub fn history(repo: &Repository, limit: usize, all_refs: bool) -> Result<Vec<CommitInfo>> {
    let mut walk = repo.revwalk()?;
    let has_head = walk.push_head().is_ok();
    if all_refs {
        let _ = walk.push_glob("refs/heads/*");
        let _ = walk.push_glob("refs/remotes/*");
        let _ = walk.push_glob("refs/tags/*");
    } else if !has_head {
        return Ok(Vec::new()); // no commits yet
    }
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        out.push(commit_info(&repo.find_commit(oid)?));
    }
    Ok(out)
}

/// The commits of one branch: a local branch, or a remote one such as
/// `origin/main`. Used by the List view to show another branch than the
/// current one, so its commits can be cherry-picked.
pub fn branch_history(repo: &Repository, limit: usize, branch: &str) -> Result<Vec<CommitInfo>> {
    let tip = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .or_else(|_| repo.find_reference(&format!("refs/remotes/{branch}")))?
        .peel_to_commit()?;
    let mut walk = repo.revwalk()?;
    walk.push(tip.id())?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        out.push(commit_info(&repo.find_commit(oid?)?));
    }
    Ok(out)
}

/// The commit is already part of the current branch (it is HEAD or behind it).
pub fn on_head(repo: &Repository, id: Oid) -> bool {
    let Some(head) = repo.head().ok().and_then(|h| h.target()) else { return false };
    head == id || repo.graph_descendant_of(head, id).unwrap_or(false)
}

fn commit_info(c: &git2::Commit) -> CommitInfo {
    let author = c.author();
    CommitInfo {
        id: c.id(),
        short_id: short(c.id()),
        summary: c.summary().ok().flatten().unwrap_or("").to_string(),
        message: c.message().unwrap_or("").trim_end().to_string(),
        author: author.name().unwrap_or("Unknown").to_string(),
        email: author.email().unwrap_or("").to_string(),
        time: c.time().seconds(),
        parents: c.parent_ids().collect(),
    }
}

/// Every branch, remote branch and tag, grouped by the commit it points at.
pub fn ref_labels(repo: &Repository) -> std::collections::HashMap<Oid, Vec<RefLabel>> {
    let mut map: std::collections::HashMap<Oid, Vec<RefLabel>> = Default::default();
    let head_name = repo
        .head()
        .ok()
        .filter(|h| h.is_branch())
        .and_then(|h| h.name().ok().map(str::to_string));
    let Ok(refs) = repo.references() else { return map };
    for r in refs.flatten() {
        let Ok(full) = r.name().map(str::to_string) else { continue };
        let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
            (RefKind::Local, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            (RefKind::Remote, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/tags/") {
            (RefKind::Tag, n.to_string())
        } else {
            continue;
        };
        let Ok(commit) = r.peel_to_commit() else { continue };
        map.entry(commit.id()).or_default().push(RefLabel {
            is_head: head_name.as_deref() == Some(full.as_str()),
            name,
            kind,
        });
    }
    for labels in map.values_mut() {
        labels.sort_by_key(|l| (!l.is_head, l.kind as u8, l.name.clone()));
    }
    map
}

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

pub fn local_branches(repo: &Repository) -> Result<Vec<BranchInfo>> {
    let mut out = Vec::new();
    for b in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = b?;
        let Some(name) = branch.name()?.map(str::to_string) else {
            continue;
        };
        let up = branch.upstream().ok();
        let upstream = up.as_ref().and_then(|u| u.name().ok().flatten().map(str::to_string));
        let (ahead, behind) = match (branch.get().target(), up.as_ref().and_then(|u| u.get().target())) {
            (Some(local), Some(remote)) => repo.graph_ahead_behind(local, remote).unwrap_or((0, 0)),
            _ => (0, 0),
        };
        out.push(BranchInfo {
            is_head: branch.is_head(),
            name,
            upstream,
            ahead,
            behind,
        });
    }
    out.sort_by_key(|b| b.name.to_lowercase());
    Ok(out)
}

pub fn remote_branches(repo: &Repository) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for b in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = b?;
        if let Some(name) = branch.name()? {
            if !name.ends_with("/HEAD") {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

pub fn checkout_branch(repo: &Repository, name: &str) -> Result<()> {
    let refname = format!("refs/heads/{name}");
    let obj = repo.revparse_single(&refname)?;
    let mut cb = git2::build::CheckoutBuilder::new();
    cb.safe();
    repo.checkout_tree(&obj, Some(&mut cb)).map_err(|e| {
        if e.code() == git2::ErrorCode::Conflict {
            anyhow!(
                "Your local changes would be overwritten by switching to '{name}'. \
                 Commit or discard them first."
            )
        } else {
            e.into()
        }
    })?;
    repo.set_head(&refname)?;
    Ok(())
}

/// Check out a remote branch such as `origin/feature` by creating (or reusing)
/// a local tracking branch called `feature`.
pub fn checkout_remote_branch(repo: &Repository, remote_name: &str) -> Result<String> {
    let local = remote_name
        .split_once('/')
        .map(|(_, b)| b.to_string())
        .unwrap_or_else(|| remote_name.to_string());
    if repo.find_branch(&local, BranchType::Local).is_err() {
        let remote = repo.find_branch(remote_name, BranchType::Remote)?;
        let commit = remote.get().peel_to_commit()?;
        let mut branch = repo.branch(&local, &commit, false)?;
        branch.set_upstream(Some(remote_name))?;
    }
    checkout_branch(repo, &local)?;
    Ok(local)
}

pub fn create_branch(repo: &Repository, name: &str, checkout: bool) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Branch name cannot be empty");
    }
    if !git2::Branch::name_is_valid(name)? {
        bail!("'{name}' is not a valid branch name");
    }
    match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(commit) => {
            repo.branch(name, &commit, false)?;
            if checkout {
                checkout_branch(repo, name)?;
            }
        }
        None => {
            // No commits yet: just point HEAD at the new (unborn) branch.
            repo.set_head(&format!("refs/heads/{name}"))?;
        }
    }
    Ok(())
}

pub fn delete_branch(repo: &Repository, name: &str) -> Result<()> {
    let mut b = repo.find_branch(name, BranchType::Local)?;
    if b.is_head() {
        bail!("You cannot delete the branch you are on");
    }
    b.delete()?;
    Ok(())
}

/// Is every commit of local branch `name` already in the current branch?
pub fn branch_merged(repo: &Repository, name: &str) -> bool {
    let Ok(b) = repo.find_branch(name, BranchType::Local) else { return false };
    let (Some(tip), Some(head)) = (b.get().target(), repo.head().ok().and_then(|h| h.target())) else {
        return false;
    };
    tip == head || repo.graph_descendant_of(head, tip).unwrap_or(false)
}

/// What merging a branch into the current branch would do, worked out
/// without touching the working folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergePreview {
    /// Commits the branch has that the current branch does not.
    pub commits: usize,
    /// The current branch has no commits of its own, so it only moves forward
    /// and no merge commit is made.
    pub fast_forward: bool,
    /// Files changed on both sides in a way Git cannot combine by itself.
    pub conflicts: usize,
}

/// Preview merging `branch` (a local branch, or a remote one such as
/// `origin/main`) into the current branch.
pub fn merge_preview(repo: &Repository, branch: &str) -> Result<MergePreview> {
    let theirs = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .or_else(|_| repo.find_reference(&format!("refs/remotes/{branch}")))?
        .peel_to_commit()?;
    let ours = repo.head()?.peel_to_commit()?;
    let (ahead, behind) = repo.graph_ahead_behind(ours.id(), theirs.id())?;
    let mut preview = MergePreview { commits: behind, fast_forward: ahead == 0 && behind > 0, conflicts: 0 };
    if ahead > 0 && behind > 0 {
        let index = repo.merge_commits(&ours, &theirs, None)?;
        let mut paths = std::collections::BTreeSet::new();
        for c in index.conflicts()?.flatten() {
            if let Some(entry) = c.our.or(c.their).or(c.ancestor) {
                paths.insert(entry.path);
            }
        }
        preview.conflicts = paths.len();
    }
    Ok(preview)
}

/// What a push of the current branch would send, and where to, worked out
/// from what the last fetch saw. Shown for review before anything is uploaded.
#[derive(Clone, Debug)]
pub struct PushPreview {
    pub local_branch: String,
    pub remote: String,
    /// Name of the branch on the remote.
    pub remote_branch: String,
    /// Address the push goes to (the push URL when the remote has one).
    pub url: String,
    /// The branch does not exist on the remote yet.
    pub new_branch: bool,
    /// Commits the push would add to the remote, newest first (at most [`PUSH_PREVIEW_LIMIT`]).
    pub commits: Vec<CommitInfo>,
    /// How many commits the push would add; more than `commits.len()` when the list was cut.
    pub total: usize,
    /// Commits the remote branch has that this branch does not. The remote
    /// refuses the push while this is not zero.
    pub behind: usize,
}

pub const PUSH_PREVIEW_LIMIT: usize = 200;

/// Preview pushing the current branch. `publish` is the (remote, branch) of a
/// first push (`git push -u <remote> <branch>`); `None` previews a plain
/// `git push` to the branch's upstream.
pub fn push_preview(repo: &Repository, publish: Option<(&str, &str)>) -> Result<PushPreview> {
    let head = repo.head().context("This repository has no commits yet")?;
    if !head.is_branch() {
        bail!("You are not on a branch");
    }
    let local_branch = head.shorthand().unwrap_or_default().to_string();
    let tip = head.peel_to_commit()?.id();
    let cfg = repo.config()?.snapshot()?;

    let (remote, remote_branch) = match publish {
        Some((remote, branch)) => (remote.to_string(), branch.to_string()),
        None => {
            let key = |k: &str| cfg.get_string(&format!("branch.{local_branch}.{k}")).ok();
            let fetch_remote = key("remote").context("This branch has no upstream")?;
            let merge = key("merge").context("This branch has no upstream")?;
            // Same order git uses to pick where `git push` goes.
            let remote = key("pushremote").or_else(|| cfg.get_string("remote.pushdefault").ok()).unwrap_or(fetch_remote);
            (remote, merge.trim_start_matches("refs/heads/").to_string())
        }
    };
    let found = repo.find_remote(&remote).with_context(|| format!("There is no remote named '{remote}'"))?;
    let url = match found.pushurl().ok().flatten() {
        Some(u) => u.to_string(),
        None => found.url().map(str::to_string).unwrap_or_default(),
    };

    let remote_tip = repo.refname_to_id(&format!("refs/remotes/{remote}/{remote_branch}")).ok();
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    match remote_tip {
        Some(id) => walk.hide(id)?,
        // A new branch: only what the remote has on none of its branches is new to it.
        None => {
            let _ = walk.hide_glob(&format!("refs/remotes/{remote}/*"));
        }
    }
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    let mut commits = Vec::new();
    let mut total = 0;
    for oid in walk {
        total += 1;
        if commits.len() < PUSH_PREVIEW_LIMIT {
            commits.push(commit_info(&repo.find_commit(oid?)?));
        }
    }
    let behind = match remote_tip {
        Some(id) => repo.graph_ahead_behind(tip, id).map(|(_, b)| b).unwrap_or(0),
        None => 0,
    };
    Ok(PushPreview { local_branch, remote, remote_branch, url, new_branch: remote_tip.is_none(), commits, total, behind })
}

/// When the last fetch from a remote ran, in seconds since the Unix epoch.
/// Git rewrites `FETCH_HEAD` on every fetch, also on one that fails.
pub fn last_fetch_time(repo: &Repository) -> Option<i64> {
    let modified = std::fs::metadata(repo.path().join("FETCH_HEAD")).ok()?.modified().ok()?;
    let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

pub fn remote_url(repo: &Repository) -> Option<String> {
    let name = default_remote(repo)?;
    let remote = repo.find_remote(&name).ok()?;
    remote.url().ok().map(str::to_string)
}

pub fn default_remote(repo: &Repository) -> Option<String> {
    let names = repo.remotes().ok()?;
    let names: Vec<String> = names.iter().filter_map(|n| n.ok().flatten().map(str::to_string)).collect();
    if names.iter().any(|n| n == "origin") {
        return Some("origin".into());
    }
    names.into_iter().next()
}

/// One configured remote. `push_url` is only set when pushes go to a
/// different address than fetches (`remote.<name>.pushurl`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
    pub push_url: Option<String>,
}

/// Every remote of the repository, the default one (see [`default_remote`]) first.
pub fn remotes(repo: &Repository) -> Vec<RemoteInfo> {
    let Ok(names) = repo.remotes() else { return Vec::new() };
    let mut out: Vec<RemoteInfo> = names
        .iter()
        .filter_map(|n| n.ok().flatten())
        .filter_map(|name| {
            let remote = repo.find_remote(name).ok()?;
            Some(RemoteInfo {
                name: name.to_string(),
                url: remote.url().map(str::to_string).unwrap_or_default(),
                push_url: remote.pushurl().ok().flatten().map(str::to_string),
            })
        })
        .collect();
    if let Some(i) = out.iter().position(|r| r.name == "origin") {
        out[..=i].rotate_right(1);
    }
    out
}

/// Tidy a remote URL typed by the user, or say what is wrong with it.
/// Any address Git understands is accepted (https, ssh, `user@host:path`, a
/// local folder, ...): only values that cannot be an address are refused.
pub fn check_remote_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty() {
        bail!("The URL is empty");
    }
    if url.starts_with('-') {
        bail!("A URL cannot start with '-'");
    }
    if url.chars().any(char::is_control) {
        bail!("The URL contains characters that are not allowed");
    }
    Ok(url.to_string())
}

fn check_remote_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("The remote needs a name, for example origin");
    }
    if name.starts_with('-') || !git2::Remote::is_valid_name(name) {
        bail!("'{name}' is not a valid remote name (use letters, digits, '-' and '_')");
    }
    Ok(name)
}

/// Add a remote and set it up to fetch all its branches.
pub fn add_remote(repo: &Repository, name: &str, url: &str) -> Result<()> {
    let name = check_remote_name(name)?;
    let url = check_remote_url(url)?;
    if repo.find_remote(name).is_ok() {
        bail!("A remote named '{name}' already exists");
    }
    repo.remote(name, &url).with_context(|| format!("Could not add the remote '{name}'"))?;
    Ok(())
}

/// Change where an existing remote points. `push_url` is the separate
/// address for pushes; `None` (or blank) means pushes use `url` too.
pub fn set_remote_url(repo: &Repository, name: &str, url: &str, push_url: Option<&str>) -> Result<()> {
    let url = check_remote_url(url)?;
    let push_url = match push_url.map(str::trim) {
        Some(p) if !p.is_empty() => Some(check_remote_url(p)?),
        _ => None,
    };
    let remote = repo.find_remote(name).with_context(|| format!("There is no remote named '{name}'"))?;
    let had_push_url = remote.pushurl_bytes().is_some();
    repo.remote_set_url(name, &url).with_context(|| format!("Could not change the URL of '{name}'"))?;
    // Clearing a push URL that is not there is an error in libgit2.
    if push_url.is_some() || had_push_url {
        repo.remote_set_pushurl(name, push_url.as_deref())
            .with_context(|| format!("Could not change the push URL of '{name}'"))?;
    }
    Ok(())
}

/// Remove a remote, with its remote-tracking branches and the upstream
/// settings of local branches that followed it. Nothing on the server changes.
pub fn remove_remote(repo: &Repository, name: &str) -> Result<()> {
    repo.remote_delete(name).with_context(|| format!("Could not remove the remote '{name}'"))?;
    Ok(())
}

/// Is `oid` an object of type commit? Used to validate selections after refresh.
pub fn commit_exists(repo: &Repository, oid: Oid) -> bool {
    repo.find_object(oid, Some(ObjectType::Commit)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> (PathBuf, Repository) {
        let dir = std::env::temp_dir().join(format!(
            "gitsot-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        // Windows CI runners set core.autocrlf=true globally; keep checkouts byte-exact.
        cfg.set_bool("core.autocrlf", false).unwrap();
        (dir, repo)
    }

    #[test]
    fn stage_commit_branch_history() {
        let (dir, repo) = tmp_repo();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();

        let st = status(&repo).unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].kind, ChangeKind::Untracked);
        assert!(!st[0].staged);

        let d = working_diff(&repo, "a.txt").unwrap();
        assert!(d.lines.iter().any(|l| l.kind == LineKind::Added && l.text == "hello"));

        stage(&repo, &["a.txt".into()]).unwrap();
        assert!(status(&repo).unwrap()[0].fully_staged());
        unstage(&repo, &["a.txt".into()]).unwrap();
        assert!(!status(&repo).unwrap()[0].staged);
        stage_all(&repo).unwrap();

        let first = commit(&repo, "First commit").unwrap();
        assert!(status(&repo).unwrap().is_empty());
        assert!(commit(&repo, "empty").is_err());

        let head = head_info(&repo);
        assert!(head.branch.is_some());

        create_branch(&repo, "feature", true).unwrap();
        assert_eq!(head_info(&repo).branch.as_deref(), Some("feature"));

        std::fs::write(dir.join("a.txt"), "hello\nworld\n").unwrap();
        stage(&repo, &["a.txt".into()]).unwrap();
        let second = commit(&repo, "Second").unwrap();

        let log = history(&repo, 10, false).unwrap();
        assert_eq!(log[0].parents, vec![first]);
        assert_eq!(history(&repo, 10, true).unwrap().len(), 2);
        let labels = ref_labels(&repo);
        assert!(labels[&second].iter().any(|l| l.name == "feature" && l.is_head));
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].id, second);
        assert_eq!(log[1].id, first);

        let files = commit_files(&repo, second).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, ChangeKind::Modified);
        let d = commit_file_diff(&repo, second, "a.txt").unwrap();
        assert!(d.lines.iter().any(|l| l.kind == LineKind::Added && l.text == "world"));

        // Discard a modification and a new file.
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let st = status(&repo).unwrap();
        assert_eq!(st.len(), 2);
        discard(&repo, &st).unwrap();
        assert!(status(&repo).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello\nworld\n");

        let main = local_branches(&repo)
            .unwrap()
            .into_iter()
            .find(|b| b.name != "feature")
            .unwrap()
            .name;
        checkout_branch(&repo, &main).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello\n");
        delete_branch(&repo, "feature").unwrap();
        assert_eq!(local_branches(&repo).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remotes_add_change_remove() {
        let (dir, repo) = tmp_repo();
        assert!(remotes(&repo).is_empty());
        assert_eq!(remote_url(&repo), None);

        add_remote(&repo, "upstream", " https://example.com/team/app.git ").unwrap();
        add_remote(&repo, "origin", "git@example.com:me/app.git").unwrap();
        let list = remotes(&repo);
        assert_eq!(list.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["origin", "upstream"]);
        assert_eq!(list[1].url, "https://example.com/team/app.git");
        assert_eq!(list[0].push_url, None);

        assert!(add_remote(&repo, "origin", "https://example.com/x.git").is_err());
        assert!(add_remote(&repo, "bad name", "https://example.com/x.git").is_err());
        assert!(add_remote(&repo, "-x", "https://example.com/x.git").is_err());
        assert!(add_remote(&repo, "other", "  ").is_err());
        assert!(add_remote(&repo, "other", "--upload-pack=evil").is_err());
        assert!(check_remote_url("https://a\nb").is_err());

        set_remote_url(&repo, "origin", "https://git.company.example/me/app.git", None).unwrap();
        assert_eq!(remote_url(&repo).as_deref(), Some("https://git.company.example/me/app.git"));
        assert!(set_remote_url(&repo, "origin", "", None).is_err());
        assert!(set_remote_url(&repo, "nope", "https://example.com/x.git", None).is_err());
        assert_eq!(remote_url(&repo).as_deref(), Some("https://git.company.example/me/app.git"));

        set_remote_url(&repo, "origin", "https://a.example/app.git", Some("git@a.example:app.git")).unwrap();
        assert_eq!(remotes(&repo)[0].push_url.as_deref(), Some("git@a.example:app.git"));
        set_remote_url(&repo, "origin", "https://a.example/app.git", Some(" ")).unwrap();
        assert_eq!(remotes(&repo)[0].push_url, None);

        remove_remote(&repo, "origin").unwrap();
        assert_eq!(default_remote(&repo).as_deref(), Some("upstream"));
        assert!(remove_remote(&repo, "origin").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
    fn write_commit(dir: &Path, repo: &Repository, file: &str, text: &str, message: &str) -> Oid {
        std::fs::write(dir.join(file), text).unwrap();
        stage(repo, &[file.into()]).unwrap();
        commit(repo, message).unwrap()
    }

    #[test]
    fn cherry_pick_preview_blocks_and_counts_conflicts() {
        let (dir, repo) = tmp_repo();
        let first = write_commit(&dir, &repo, "a.txt", "one\n", "First");
        let main = head_info(&repo).branch.unwrap();

        create_branch(&repo, "feature", true).unwrap();
        let clean = write_commit(&dir, &repo, "b.txt", "b\n", "Add b");
        let clash = write_commit(&dir, &repo, "a.txt", "feature\n", "Change a");
        checkout_branch(&repo, &main).unwrap();
        write_commit(&dir, &repo, "a.txt", "main\n", "Main change");

        assert_eq!(cherry_pick_preview(&repo, clean).unwrap(), CherryPickPreview::default());
        assert_eq!(cherry_pick_preview(&repo, clash).unwrap().conflicts, 1);
        // Already on this branch.
        assert!(cherry_pick_preview(&repo, first).unwrap().blocker.is_some());
        assert!(on_head(&repo, first) && !on_head(&repo, clean));
        let ids: Vec<Oid> = branch_history(&repo, 10, "feature").unwrap().iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![clash, clean, first]);
        // Nothing was touched by previewing.
        assert!(status(&repo).unwrap().is_empty());
        assert_eq!(repo.state(), git2::RepositoryState::Clean);

        // Untracked files are fine, changes to tracked files are not.
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        assert!(cherry_pick_preview(&repo, clean).unwrap().blocker.is_none());
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        let blocker = cherry_pick_preview(&repo, clean).unwrap().blocker.unwrap();
        assert!(blocker.contains("uncommitted"), "{blocker}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_finishes_a_cherry_pick_with_the_original_author() {
        let (dir, repo) = tmp_repo();
        write_commit(&dir, &repo, "a.txt", "one\n", "First");
        let main = head_info(&repo).branch.unwrap();
        create_branch(&repo, "feature", true).unwrap();
        repo.config().unwrap().set_str("user.email", "other@example.com").unwrap();
        let picked = write_commit(&dir, &repo, "a.txt", "feature\n", "Change a");
        repo.config().unwrap().set_str("user.email", "test@example.com").unwrap();
        checkout_branch(&repo, &main).unwrap();
        write_commit(&dir, &repo, "a.txt", "main\n", "Main change");

        // Start the cherry-pick the way git does: it stops on the conflict.
        repo.cherrypick(&repo.find_commit(picked).unwrap(), None).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::CherryPick);
        assert_eq!(cherry_pick_head(&repo), Some(picked));
        assert!(commit(&repo, "Change a").is_err(), "conflicts must be resolved first");

        std::fs::write(dir.join("a.txt"), "both\n").unwrap();
        stage(&repo, &["a.txt".into()]).unwrap();
        let done = repo.find_commit(commit(&repo, "Change a").unwrap()).unwrap();
        assert_eq!(done.author().email().ok(), Some("other@example.com"));
        assert_eq!(done.committer().email().ok(), Some("test@example.com"));
        assert_eq!(done.parent_count(), 1);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_preview_counts_commits_and_conflicts() {
        let (dir, repo) = tmp_repo();
        write_commit(&dir, &repo, "a.txt", "one\n", "First");
        let main = head_info(&repo).branch.unwrap();

        // Two commits on a branch, none on the current one: a fast-forward.
        create_branch(&repo, "feature", true).unwrap();
        write_commit(&dir, &repo, "a.txt", "feature\n", "Feature 1");
        write_commit(&dir, &repo, "b.txt", "b\n", "Feature 2");
        checkout_branch(&repo, &main).unwrap();
        assert_eq!(merge_preview(&repo, "feature").unwrap(), MergePreview { commits: 2, fast_forward: true, conflicts: 0 });

        // A commit of our own in another file: a real merge, no conflicts.
        write_commit(&dir, &repo, "c.txt", "c\n", "Main 1");
        assert_eq!(merge_preview(&repo, "feature").unwrap(), MergePreview { commits: 2, fast_forward: false, conflicts: 0 });

        // The same file changed on both sides: one conflicted file.
        write_commit(&dir, &repo, "a.txt", "main\n", "Main 2");
        assert_eq!(merge_preview(&repo, "feature").unwrap(), MergePreview { commits: 2, fast_forward: false, conflicts: 1 });

        // Nothing was touched by previewing.
        assert!(status(&repo).unwrap().is_empty());
        assert_eq!(repo.state(), git2::RepositoryState::Clean);

        // Everything of the other side is already here: nothing to merge.
        checkout_branch(&repo, "feature").unwrap();
        create_branch(&repo, "old", false).unwrap();
        write_commit(&dir, &repo, "d.txt", "d\n", "Feature 3");
        assert_eq!(merge_preview(&repo, "old").unwrap(), MergePreview::default());
        assert!(merge_preview(&repo, "missing").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn branches_count_ahead_and_behind_their_upstream() {
        let (dir, repo) = tmp_repo();
        let first = write_commit(&dir, &repo, "a.txt", "one\n", "First");
        let main = head_info(&repo).branch.unwrap();
        // Pretend the remote is two commits further than where we started.
        write_commit(&dir, &repo, "a.txt", "two\n", "Remote 1");
        let tip = write_commit(&dir, &repo, "a.txt", "three\n", "Remote 2");
        add_remote(&repo, "origin", "https://example.com/a/b.git").unwrap();
        repo.reference(&format!("refs/remotes/origin/{main}"), tip, true, "test").unwrap();
        repo.find_branch(&main, BranchType::Local).unwrap().set_upstream(Some(&format!("origin/{main}"))).unwrap();
        // Our branch goes back to the first commit and gets one commit of its own.
        repo.reset(&repo.find_object(first, None).unwrap(), git2::ResetType::Hard, None).unwrap();
        write_commit(&dir, &repo, "b.txt", "mine\n", "Mine");

        let branch = local_branches(&repo).unwrap().into_iter().find(|b| b.name == main).unwrap();
        assert_eq!((branch.ahead, branch.behind), (1, 2));
        let head = head_info(&repo);
        assert_eq!((head.ahead, head.behind), (1, 2));
        assert_eq!(merge_preview(&repo, &format!("origin/{main}")).unwrap().commits, 2);
        assert!(last_fetch_time(&repo).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_preview_lists_what_the_remote_does_not_have() {
        let (dir, repo) = tmp_repo();
        let first = write_commit(&dir, &repo, "a.txt", "one\n", "First");
        let main = head_info(&repo).branch.unwrap();
        // Nothing to preview without an upstream, unless the branch is being published.
        assert!(push_preview(&repo, None).is_err());
        add_remote(&repo, "origin", "https://example.com/a/b.git").unwrap();
        let publish = push_preview(&repo, Some(("origin", &main))).unwrap();
        assert!(publish.new_branch);
        assert_eq!((publish.total, publish.behind), (1, 0));
        assert_eq!(publish.url, "https://example.com/a/b.git");

        // The remote has the first commit and one we do not have; we add two.
        let theirs = write_commit(&dir, &repo, "a.txt", "two\n", "Remote");
        repo.reference(&format!("refs/remotes/origin/{main}"), theirs, true, "test").unwrap();
        repo.find_branch(&main, BranchType::Local).unwrap().set_upstream(Some(&format!("origin/{main}"))).unwrap();
        repo.reset(&repo.find_object(first, None).unwrap(), git2::ResetType::Hard, None).unwrap();
        write_commit(&dir, &repo, "b.txt", "mine\n", "Mine 1");
        let tip = write_commit(&dir, &repo, "b.txt", "more\n", "Mine 2");
        set_remote_url(&repo, "origin", "https://example.com/a/b.git", Some("https://push.example.com/a/b.git")).unwrap();

        let p = push_preview(&repo, None).unwrap();
        assert_eq!((p.local_branch.as_str(), p.remote.as_str(), p.remote_branch.as_str()), (main.as_str(), "origin", main.as_str()));
        assert_eq!(p.url, "https://push.example.com/a/b.git");
        assert!(!p.new_branch);
        assert_eq!((p.total, p.behind), (2, 1));
        assert_eq!(p.commits[0].id, tip);
        assert_eq!(p.commits[0].email, "test@example.com");
        assert_eq!(p.commits.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), ["Mine 2", "Mine 1"]);

        // A new branch only sends what the remote has on none of its branches.
        create_branch(&repo, "feature", true).unwrap();
        write_commit(&dir, &repo, "c.txt", "new\n", "Feature");
        let p = push_preview(&repo, Some(("origin", "feature"))).unwrap();
        assert!(p.new_branch);
        assert_eq!(p.total, 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

}
