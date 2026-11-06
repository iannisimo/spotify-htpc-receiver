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

    /// Switches between sorted and shuffled order without changing the current preset.
    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        let current = self.order[self.pos];
        self.order = (0..self.presets.len()).collect();
        self.pos = current;
        if on {
            self.reshuffle(Some(current));
            // The preset on screen leads the new pass.
            let at = self.order.iter().position(|&i| i == current).unwrap_or(0);
            self.order.swap(0, at);
            self.pos = 0;
        }
    }

    /// Keeps only the presets for which `keep` is true. The current preset stays current if
    /// it is kept, else the next kept one in the pass takes over. Returns false, and changes
    /// nothing, when no preset would be left.
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) -> bool {
        let flags: Vec<bool> = self.presets.iter().map(|p| keep(p)).collect();
        if !flags.contains(&true) {
            return false;
        }
        let len = self.order.len();
        let first_kept = (self.pos..len)
            .chain(0..self.pos)
            .find(|&p| flags[self.order[p]])
            .unwrap_or(0);
        let pos = self.order[..first_kept]
            .iter()
            .filter(|&&i| flags[i])
            .count();
        let mut new_index = vec![0; flags.len()];
        let mut next = 0;
        for (i, &kept) in flags.iter().enumerate() {
            new_index[i] = next;
            next += usize::from(kept);
        }
        self.order = self
            .order
            .iter()
            .filter(|&&i| flags[i])
            .map(|&i| new_index[i])
            .collect();
        let mut flags = flags.into_iter();
        self.presets.retain(|_| flags.next().unwrap_or(false));
        self.pos = pos;
        true
    }

    /// Where `path` is in the current pass, if it is in the list.
    pub fn position_of(&self, path: &Path) -> Option<usize> {
        self.order
            .iter()
            .position(|&i| self.presets[i].as_path() == path)
    }

    /// Makes the preset at `index` of the pass current (clamped).
    pub fn seek(&mut self, index: usize) {
        self.pos = index.min(self.order.len() - 1);
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
    fn playlist_retain_and_shuffle() {
        let mut list = Playlist::new(paths(&["a", "b", "c", "d"]), false, 1).unwrap();
        list.forward(); // b
        assert!(list.retain(|p| p != Path::new("a") && p != Path::new("c")));
        assert_eq!(list.count(), 2);
        assert_eq!(list.current(), Path::new("b"), "kept preset stays current");
        assert_eq!(names(&mut list, 2), ["d", "b"]);

        // The current preset is dropped: the next kept one takes over.
        assert!(list.retain(|p| p == Path::new("d")));
        assert_eq!(list.current(), Path::new("d"));

        // Nothing left: refused and unchanged.
        assert!(!list.retain(|_| false));
        assert_eq!(list.count(), 1);
        assert_eq!(list.current(), Path::new("d"));

        let mut list = Playlist::new(paths(&["a", "b", "c", "d"]), true, 5).unwrap();
        list.forward();
        let shown = list.current().to_owned();
        list.set_shuffle(false);
        assert_eq!(
            list.current(),
            shown,
            "same preset after switching to sorted"
        );
        let sorted: Vec<String> = names(&mut list, 4);
        let mut expect = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        let at = expect.iter().position(|n| Path::new(n) == shown).unwrap();
        expect.rotate_left(at + 1);
        assert_eq!(sorted, expect, "sorted from the current preset");
        list.set_shuffle(true);
        let again = list.current().to_owned();
        assert_eq!(list.position_of(&again), Some(0));
        list.seek(2);
        assert_eq!(list.position_of(list.current()), Some(2));
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
