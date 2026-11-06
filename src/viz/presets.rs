//! Finding `.milk` presets and choosing which one plays next.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where presets live inside a data directory, as projectM's own packages install them.
const DATA_SUBDIR: &str = "projectM/presets";
/// How deep to look below a preset directory (also stops symlink loops).
const MAX_DEPTH: u32 = 8;

/// Directories searched when the config names none: the user's data directory first, then the
/// system ones (XDG base directory rules).
pub fn default_dirs(
    data_home: Option<OsString>,
    data_dirs: Option<OsString>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let absolute = |p: PathBuf| p.is_absolute().then_some(p);
    let home_data = data_home
        .map(PathBuf::from)
        .and_then(absolute)
        .or_else(|| home.map(|h| h.join(".local/share")));
    let system: Vec<PathBuf> = data_dirs
        .filter(|d| !d.is_empty())
        .map(|d| std::env::split_paths(&d).filter_map(absolute).collect())
        .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
    home_data
        .into_iter()
        .chain(system)
        .map(|d| d.join(DATA_SUBDIR))
        .collect()
}

/// Every `.milk` file under `dir`, sorted. A missing or unreadable directory gives none.
pub fn find(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(dir, 0, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                collect(&path, depth + 1, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("milk"))
        {
            out.push(path);
        }
    }
}

/// The preset order: sorted, or shuffled anew on every pass through the list.
#[derive(Debug)]
pub struct Playlist {
    presets: Vec<PathBuf>,
    /// Indices into `presets` for the current pass.
    order: Vec<usize>,
    pos: usize,
    shuffle: bool,
    rng: u64,
}

impl Playlist {
    /// `None` when there are no presets.
    pub fn new(presets: Vec<PathBuf>, shuffle: bool, seed: u64) -> Option<Playlist> {
        if presets.is_empty() {
            return None;
        }
        let mut list = Playlist {
            order: (0..presets.len()).collect(),
            presets,
            pos: 0,
            shuffle,
            rng: seed | 1, // xorshift must not start at 0
        };
        if shuffle {
            list.reshuffle(None);
        }
        Some(list)
    }

    pub fn count(&self) -> usize {
        self.presets.len()
    }

    pub fn current(&self) -> &Path {
        &self.presets[self.order[self.pos]]
    }

    /// File name without `.milk`.
    pub fn current_name(&self) -> String {
        let path = self.current();
        path.file_stem()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    pub fn forward(&mut self) -> &Path {
        self.pos += 1;
        if self.pos == self.order.len() {
            self.pos = 0;
            if self.shuffle {
                let last = self.order.last().copied();
                self.reshuffle(last);
            }
        }
        self.current()
    }

    pub fn back(&mut self) -> &Path {
        self.pos = self.pos.checked_sub(1).unwrap_or(self.order.len() - 1);
        self.current()
    }

    /// Drops the current preset (it failed to load) and moves to the next one. `None` when
    /// none are left.
    pub fn remove_current(&mut self) -> Option<&Path> {
        if self.presets.len() == 1 {
            return None;
        }
        let gone = self.order.remove(self.pos);
        self.presets.remove(gone);
        for i in &mut self.order {
            if *i > gone {
                *i -= 1;
            }
        }
        if self.pos == self.order.len() {
            self.pos = 0;
        }
        Some(self.current())
    }

    /// Fisher–Yates; `avoid_first` keeps the preset just shown from playing again right away.
    fn reshuffle(&mut self, avoid_first: Option<usize>) {
        for i in (1..self.order.len()).rev() {
            let j = (self.random() % (i as u64 + 1)) as usize;
            self.order.swap(i, j);
        }
        if self.order.len() > 1 && self.order.first().copied() == avoid_first {
            self.order.swap(0, 1);
        }
    }

    fn random(&mut self) -> u64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn names(list: &mut Playlist, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| list.forward().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn sorted_order_wraps_both_ways() {
        let mut list = Playlist::new(paths(&["a", "b", "c"]), false, 1).unwrap();
        assert_eq!(list.current(), Path::new("a"));
        assert_eq!(names(&mut list, 3), ["b", "c", "a"]);
        assert_eq!(list.back(), Path::new("c"));
        assert_eq!(list.back(), Path::new("b"));
    }

    #[test]
    fn empty_list_is_none() {
        assert!(Playlist::new(Vec::new(), true, 1).is_none());
    }

    #[test]
    fn shuffle_plays_every_preset_once_per_pass() {
        let all: Vec<String> = (0..20).map(|i| format!("p{i:02}")).collect();
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let mut list = Playlist::new(paths(&refs), true, 42).unwrap();
        for _ in 0..3 {
            let pass: Vec<String> = std::iter::once(list.current().to_string_lossy().into_owned())
                .chain(names(&mut list, 19))
                .collect();
            assert_eq!(pass.iter().collect::<HashSet<_>>().len(), 20, "{pass:?}");
            list.forward();
        }
    }

    #[test]
    fn shuffle_changes_the_order() {
        let all: Vec<String> = (0..20).map(|i| format!("p{i:02}")).collect();
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let mut list = Playlist::new(paths(&refs), true, 7).unwrap();
        assert_ne!(names(&mut list, 19), all[1..]);
    }

    #[test]
    fn shuffle_never_repeats_across_a_pass_boundary() {
        for seed in 0..200 {
            let mut list = Playlist::new(paths(&["a", "b", "c"]), true, seed).unwrap();
            let mut prev = list.current().to_owned();
            for _ in 0..12 {
                let next = list.forward().to_owned();
                assert_ne!(next, prev, "seed {seed}");
                prev = next;
            }
        }
    }

    #[test]
    fn single_preset_repeats() {
        let mut list = Playlist::new(paths(&["only"]), true, 3).unwrap();
        assert_eq!(list.forward(), Path::new("only"));
        assert_eq!(list.back(), Path::new("only"));
    }

    #[test]
    fn remove_current_moves_on_and_keeps_the_rest() {
        let mut list = Playlist::new(paths(&["a", "b", "c"]), false, 1).unwrap();
        list.forward(); // b
        assert_eq!(list.remove_current(), Some(Path::new("c")));
        assert_eq!(list.count(), 2);
        assert_eq!(names(&mut list, 2), ["a", "c"]);
        // Removing the last in the order wraps to the first.
        assert_eq!(list.remove_current(), Some(Path::new("a")));
        assert_eq!(list.remove_current(), None);
        assert_eq!(list.current(), Path::new("a"));
    }

    #[test]
    fn current_name_drops_the_extension() {
        let list = Playlist::new(paths(&["/x/Geiss - Spiral.milk"]), false, 1).unwrap();
        assert_eq!(list.current_name(), "Geiss - Spiral");
    }

    #[test]
    fn find_is_recursive_sorted_and_milk_only() {
        let dir = std::env::temp_dir().join(format!("htpc-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Fractal/deep")).unwrap();
        for f in [
            "b.milk",
            "a.MILK",
            "notes.txt",
            "Fractal/c.milk",
            "Fractal/deep/d.milk",
        ] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let found: Vec<_> = find(&dir)
            .into_iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_owned())
            .collect();
        assert_eq!(
            found,
            paths(&["Fractal/c.milk", "Fractal/deep/d.milk", "a.MILK", "b.milk"])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_in_missing_dir_is_empty() {
        assert!(find(Path::new("/nonexistent/htpc/presets")).is_empty());
    }

    #[test]
    fn default_dirs_follow_xdg() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            default_dirs(None, None, home.clone()),
            paths(&[
                "/home/me/.local/share/projectM/presets",
                "/usr/local/share/projectM/presets",
                "/usr/share/projectM/presets",
            ])
        );
        assert_eq!(
            default_dirs(
                Some("/data".into()),
                Some("/opt/share:relative:/usr/share".into()),
                home,
            ),
            paths(&[
                "/data/projectM/presets",
                "/opt/share/projectM/presets",
                "/usr/share/projectM/presets",
            ])
        );
    }
}
