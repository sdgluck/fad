//! Things you never want ranked, remembered between runs.
//!
//! Everyone has directories they will not delete and do not want to scroll past
//! every session: a photo library, a work VM, a mounted archive. Ignoring is
//! deliberately *not* a way to make a number smaller — an ignored entry still
//! counts towards every total above it, because a size that quietly omits
//! things is the one failure this tool cannot afford. It is hidden from the
//! views, counted in a banner, and refused for deletion.
//!
//! The file is a subset of gitignore, chosen so that what it does is guessable
//! from what it looks like:
//!
//! ```text
//! # a comment
//! /Users/you/VMs           an absolute path, and everything under it
//! ~/Pictures/Photos.photoslibrary
//! *.sparsebundle           a glob, matched against each entry's own name
//! Steam/                   a glob, directories only
//! ```

use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Rules {
    absolute: Vec<PathBuf>,
    names: Vec<Name>,
}

struct Name {
    pattern: String,
    dirs_only: bool,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.absolute.is_empty() && self.names.is_empty()
    }

    pub fn parse(text: &str) -> Rules {
        let mut rules = Rules::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (body, dirs_only) = match line.strip_suffix('/') {
                Some(b) => (b, true),
                None => (line, false),
            };
            if let Some(rest) = body.strip_prefix("~/") {
                if let Some(home) = crate::paths::home() {
                    rules.absolute.push(home.join(rest));
                    continue;
                }
            }
            if body.starts_with('/') {
                rules.absolute.push(PathBuf::from(body));
            } else {
                rules.names.push(Name { pattern: body.to_string(), dirs_only });
            }
        }
        rules
    }

    /// Read the user's list. A missing file is the normal case, not an error.
    pub fn load() -> Rules {
        let Some(path) = path() else { return Rules::default() };
        match std::fs::read_to_string(path) {
            Ok(text) => Rules::parse(&text),
            Err(_) => Rules::default(),
        }
    }

    pub fn matches(&self, path: &Path, name: &str, is_dir: bool) -> bool {
        if self.absolute.iter().any(|p| path.starts_with(p)) {
            return true;
        }
        self.names
            .iter()
            .any(|n| (is_dir || !n.dirs_only) && glob(&n.pattern, name))
    }

    /// Add a path to the file, and to these rules, so the effect is immediate
    /// and outlives the session. Returns where it was written.
    pub fn add(&mut self, path: &Path) -> std::io::Result<PathBuf> {
        use std::io::Write;

        let Some(file) = self::path() else {
            return Err(std::io::Error::other("no home directory to write a config into"));
        };
        std::fs::create_dir_all(file.parent().unwrap())?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
        writeln!(f, "{}", path.display())?;
        self.absolute.push(path.to_path_buf());
        Ok(file)
    }
}

/// `$XDG_CONFIG_HOME/fad/ignore`, or `~/.config/fad/ignore`, on both platforms.
/// This one file is meant to be opened and edited by hand, and nobody goes
/// looking in `~/Library/Application Support` to do that.
pub fn path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FAD_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("ignore"));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => crate::paths::home()?.join(".config"),
    };
    Some(base.join("fad").join("ignore"))
}

/// `*` and `?`, matched against one path component. Backtracking on `*` only,
/// which is all this grammar needs and keeps the whole thing in one screen.
fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have matched too little.
    let (mut star, mut resume) = (None, 0usize);

    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn globs_match_one_component() {
        assert!(glob("*.sparsebundle", "backup.sparsebundle"));
        assert!(!glob("*.sparsebundle", "backup.sparsebundle.old"));
        assert!(glob("node_modules", "node_modules"));
        assert!(!glob("node_modules", "node_modules2"));
        assert!(glob("*", "anything"));
        assert!(glob("a*c", "abbbc"));
        assert!(!glob("a*c", "abbbd"));
        assert!(glob("?ar", "bar"));
        assert!(!glob("?ar", "bbar"));
        // The backtracking case: the first `*` must give ground.
        assert!(glob("*a*b", "xxaxxb"));
        assert!(!glob("*a*b", "xxaxxc"));
    }
}
