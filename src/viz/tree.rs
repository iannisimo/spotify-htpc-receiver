//! The preset folder as a tree with checkboxes, for the settings panel.
//!
//! Nodes live in one arena in pre-order (a parent always comes before its children), which makes
//! the counts and the visible rows single passes over a `Vec`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::disabled::DisabledPresets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    On,
    Off,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderInfo {
    pub expanded: bool,
    pub total: usize,
    pub off: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: u32,
    pub label: String,
    pub check: Check,
    /// `Some` for folders.
    pub folder: Option<FolderInfo>,
    pub focused: bool,
    pub demo: bool,
}

#[derive(Debug)]
enum Kind {
    Folder {
        expanded: bool,
        total: usize,
        off: usize,
    },
    Preset {
        enabled: bool,
        path: PathBuf,
    },
}

#[derive(Debug)]
struct Node {
    name: String,
    /// Path below the preset folder, `/`-separated, no trailing slash. Empty for the root.
    rel: String,
    parent: usize,
    depth: u32,
    children: Vec<usize>,
    kind: Kind,
}

#[derive(Debug)]
pub struct PresetTree {
    /// Node 0 is the (never shown) root.
    nodes: Vec<Node>,
    by_path: HashMap<PathBuf, usize>,
    /// The node under the cursor (never the root unless the tree is empty).
    cursor: usize,
    demo: Option<usize>,
}

/// Intermediate shape used only while building.
#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: BTreeMap<String, PathBuf>,
}

impl PresetTree {
    /// `presets` are absolute paths below `root`. Folders come before presets, both sorted by
    /// name; every folder starts collapsed and the cursor on the first row.
    pub fn build(root: &Path, presets: &[PathBuf], disabled: &DisabledPresets) -> PresetTree {
        let mut top = Dir::default();
        for path in presets {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let Some((file, folders)) = parts.split_last() else {
                continue;
            };
            let mut dir = &mut top;
            for folder in folders {
                dir = dir.dirs.entry(folder.clone()).or_default();
            }
            dir.files.insert(file.clone(), path.clone());
        }

        let mut tree = PresetTree {
            nodes: vec![Node {
                name: String::new(),
                rel: String::new(),
                parent: 0,
                depth: 0,
                children: Vec::new(),
                kind: Kind::Folder {
                    expanded: true,
                    total: 0,
                    off: 0,
                },
            }],
            by_path: HashMap::new(),
            cursor: 0,
            demo: None,
        };
        tree.add_dir(0, &top, disabled);
        tree.recount();
        tree.cursor = tree.visible().first().copied().unwrap_or(0);
        tree
    }

    fn add_dir(&mut self, parent: usize, dir: &Dir, disabled: &DisabledPresets) {
        let depth = self.nodes[parent].depth + u32::from(parent != 0);
        let prefix = if parent == 0 {
            String::new()
        } else {
            format!("{}/", self.nodes[parent].rel)
        };
        for (name, sub) in &dir.dirs {
            let id = self.push(
                parent,
                name,
                format!("{prefix}{name}"),
                depth,
                Kind::Folder {
                    expanded: false,
                    total: 0,
                    off: 0,
                },
            );
            self.add_dir(id, sub, disabled);
        }
        for (name, path) in &dir.files {
            let rel = format!("{prefix}{name}");
            let enabled = !disabled.is_disabled(Path::new(&rel));
            let id = self.push(
                parent,
                name,
                rel,
                depth,
                Kind::Preset {
                    enabled,
                    path: path.clone(),
                },
            );
            self.by_path.insert(path.clone(), id);
        }
    }

    fn push(&mut self, parent: usize, name: &str, rel: String, depth: u32, kind: Kind) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_owned(),
            rel,
            parent,
            depth,
            children: Vec::new(),
            kind,
        });
        self.nodes[parent].children.push(id);
        id
    }

    /// Recomputes every folder's totals from the presets, in one pass (children have higher
    /// indices than their parents).
    fn recount(&mut self) {
        for node in &mut self.nodes {
            if let Kind::Folder { total, off, .. } = &mut node.kind {
                *total = 0;
                *off = 0;
            }
        }
        for id in (1..self.nodes.len()).rev() {
            let (t, o) = match self.nodes[id].kind {
                Kind::Preset { enabled, .. } => (1, usize::from(!enabled)),
                Kind::Folder { total, off, .. } => (total, off),
            };
            let parent = self.nodes[id].parent;
            if let Kind::Folder { total, off, .. } = &mut self.nodes[parent].kind {
                *total += t;
                *off += o;
            }
        }
    }

    /// Node ids of the rows currently shown, top to bottom.
    fn visible(&self) -> Vec<usize> {
        let mut shown = vec![false; self.nodes.len()];
        shown[0] = true;
        let mut rows = Vec::new();
        for id in 1..self.nodes.len() {
            let parent = self.nodes[id].parent;
            let parent_open =
                matches!(self.nodes[parent].kind, Kind::Folder { expanded: true, .. });
            if shown[parent] && parent_open {
                shown[id] = true;
                rows.push(id);
            }
        }
        rows
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let rows = self.visible();
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|&id| id == self.cursor).unwrap_or(0);
        self.cursor = rows[at.saturating_add_signed(delta).min(rows.len() - 1)];
    }

    pub fn expand(&mut self) {
        if let Kind::Folder { expanded, .. } = &mut self.nodes[self.cursor].kind
            && self.cursor != 0
        {
            *expanded = true;
        }
    }

    /// Collapses the folder under the cursor; on a preset or an already collapsed folder, moves
    /// to the parent folder instead.
    pub fn collapse(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Kind::Folder { expanded, .. } = &mut self.nodes[self.cursor].kind
            && *expanded
        {
            *expanded = false;
            return;
        }
        let parent = self.nodes[self.cursor].parent;
        if parent != 0 {
            self.cursor = parent;
        }
    }

    /// A preset: flips it. A folder that is `Mixed` or `On` goes all off; one that is `Off`
    /// goes all on.
    pub fn toggle(&mut self) {
        let id = self.cursor;
        if id == 0 {
            return;
        }
        let enable = match self.nodes[id].kind {
            Kind::Preset { enabled, .. } => !enabled,
            Kind::Folder { total, off, .. } => off == total,
        };
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            match &mut self.nodes[n].kind {
                Kind::Preset { enabled, .. } => *enabled = enable,
                Kind::Folder { .. } => stack.extend(self.nodes[n].children.iter().copied()),
            }
        }
        self.recount();
    }

    /// The preset under the cursor, `None` on a folder.
    pub fn cursor_preset(&self) -> Option<PathBuf> {
        match &self.nodes[self.cursor].kind {
            Kind::Preset { path, .. } => Some(path.clone()),
            Kind::Folder { .. } => None,
        }
    }

    pub fn set_demo(&mut self, path: Option<PathBuf>) {
        self.demo = path.and_then(|p| self.by_path.get(&p).copied());
    }

    /// Presets the tree does not know count as enabled.
    pub fn is_enabled(&self, path: &Path) -> bool {
        match self.by_path.get(path).map(|&id| &self.nodes[id].kind) {
            Some(Kind::Preset { enabled, .. }) => *enabled,
            _ => true,
        }
    }

    /// At most `window` rows, scrolled so the cursor stays in view.
    pub fn visible_rows(&self, window: usize) -> Vec<TreeRow> {
        let rows = self.visible();
        let at = rows.iter().position(|&id| id == self.cursor).unwrap_or(0);
        let start = at
            .saturating_sub(window / 2)
            .min(rows.len().saturating_sub(window));
        rows.iter()
            .skip(start)
            .take(window)
            .map(|&id| self.row(id))
            .collect()
    }

    fn row(&self, id: usize) -> TreeRow {
        let node = &self.nodes[id];
        let (label, check, folder) = match &node.kind {
            Kind::Preset { enabled, .. } => {
                let stem = Path::new(&node.name)
                    .file_stem()
                    .map_or_else(|| node.name.clone(), |s| s.to_string_lossy().into_owned());
                (stem, if *enabled { Check::On } else { Check::Off }, None)
            }
            Kind::Folder {
                expanded,
                total,
                off,
            } => {
                let label = if *off == 0 {
                    format!("{} ({total})", node.name)
                } else {
                    format!("{} ({total}, {off} off)", node.name)
                };
                let check = match *off {
                    0 => Check::On,
                    n if n == *total => Check::Off,
                    _ => Check::Mixed,
                };
                (
                    label,
                    check,
                    Some(FolderInfo {
                        expanded: *expanded,
                        total: *total,
                        off: *off,
                    }),
                )
            }
        };
        TreeRow {
            depth: node.depth,
            label,
            check,
            folder,
            focused: id == self.cursor,
            demo: self.demo == Some(id),
        }
    }

    /// `(total, off)` over all presets.
    pub fn counts(&self) -> (usize, usize) {
        match self.nodes[0].kind {
            Kind::Folder { total, off, .. } => (total, off),
            Kind::Preset { .. } => (0, 0),
        }
    }

    /// The set to save: a fully-off folder becomes one `folder/` line, otherwise one line per
    /// off preset. Lines of `previous` that match nothing in this tree are kept.
    pub fn to_disabled(&self, previous: &DisabledPresets) -> DisabledPresets {
        let mut out = DisabledPresets::default();
        let mut known: HashSet<String> = HashSet::new();
        let mut stack: Vec<usize> = self.nodes[0].children.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            match node.kind {
                Kind::Preset { enabled, .. } => {
                    known.insert(node.rel.clone());
                    if !enabled {
                        out.insert(node.rel.clone());
                    }
                }
                Kind::Folder { total, off, .. } => {
                    known.insert(format!("{}/", node.rel));
                    if total > 0 && off == total {
                        out.insert(format!("{}/", node.rel));
                        self.mark_known(id, &mut known);
                    } else {
                        stack.extend(node.children.iter().rev().copied());
                    }
                }
            }
        }
        for entry in previous.entries() {
            if !known.contains(entry) {
                out.insert(entry);
            }
        }
        out
    }

    /// Records every path below `folder` as known (its lines in the old file are replaced by
    /// the folder line).
    fn mark_known(&self, folder: usize, known: &mut HashSet<String>) {
        let mut stack = vec![folder];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            match node.kind {
                Kind::Preset { .. } => {
                    known.insert(node.rel.clone());
                }
                Kind::Folder { .. } => {
                    known.insert(format!("{}/", node.rel));
                    stack.extend(node.children.iter().copied());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &str, rels: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let root = PathBuf::from(root);
        let list = rels.iter().map(|r| root.join(r)).collect();
        (root, list)
    }

    fn sample(disabled: &DisabledPresets) -> PresetTree {
        let (root, list) = paths(
            "/p",
            &["A/a1.milk", "A/sub/a2.milk", "B/b1.milk", "top.milk"],
        );
        PresetTree::build(&root, &list, disabled)
    }

    fn labels(tree: &PresetTree) -> Vec<String> {
        tree.visible_rows(100)
            .into_iter()
            .map(|r| r.label)
            .collect()
    }

    #[test]
    fn builds_collapsed_folders_first() {
        let tree = sample(&DisabledPresets::default());
        assert_eq!(labels(&tree), ["A (2)", "B (1)", "top"]);
        assert_eq!(tree.counts(), (4, 0));
        let rows = tree.visible_rows(10);
        assert!(rows[0].focused && !rows[1].focused);
        assert_eq!(rows[0].folder.map(|f| f.expanded), Some(false));
        assert_eq!(rows[2].folder, None);
    }

    #[test]
    fn expand_collapse_and_parent() {
        let mut tree = sample(&DisabledPresets::default());
        tree.expand();
        assert_eq!(labels(&tree), ["A (2)", "sub (1)", "a1", "B (1)", "top"]);
        tree.move_cursor(2);
        assert_eq!(tree.cursor_preset(), Some(PathBuf::from("/p/A/a1.milk")));
        tree.collapse();
        assert_eq!(tree.cursor_preset(), None);
        assert!(tree.visible_rows(10)[0].focused, "cursor moved to A");
        tree.collapse();
        assert_eq!(labels(&tree), ["A (2)", "B (1)", "top"]);
        let depths: Vec<u32> = {
            tree.expand();
            tree.visible_rows(10).iter().map(|r| r.depth).collect()
        };
        assert_eq!(depths, [0, 1, 1, 0, 0]);
    }

    #[test]
    fn toggle_folder_sets_all_and_mixed_state() {
        let mut tree = sample(&DisabledPresets::default());
        tree.expand();
        tree.move_cursor(1); // sub
        tree.toggle();
        let rows = tree.visible_rows(10);
        assert_eq!(rows[0].check, Check::Mixed, "A has one off, one on");
        assert_eq!(rows[1].check, Check::Off);
        assert_eq!(rows[1].label, "sub (1, 1 off)");
        tree.move_cursor(-1);
        tree.toggle();
        assert!(tree.visible_rows(10)[0].check == Check::Off);
        assert_eq!(tree.counts(), (4, 2));
        tree.toggle();
        assert!(tree.visible_rows(10)[0].check == Check::On);
        assert_eq!(tree.counts(), (4, 0));
    }

    #[test]
    fn toggle_preset_flips_it() {
        let mut tree = sample(&DisabledPresets::default());
        tree.move_cursor(2);
        tree.toggle();
        assert!(!tree.is_enabled(Path::new("/p/top.milk")));
        assert!(tree.is_enabled(Path::new("/p/B/b1.milk")));
        assert!(tree.is_enabled(Path::new("/p/unknown.milk")));
        tree.toggle();
        assert!(tree.is_enabled(Path::new("/p/top.milk")));
    }

    #[test]
    fn cursor_preset_and_demo_mark() {
        let mut tree = sample(&DisabledPresets::default());
        tree.expand();
        assert_eq!(tree.cursor_preset(), None);
        tree.move_cursor(2);
        let a1 = tree.cursor_preset().unwrap();
        assert_eq!(a1, PathBuf::from("/p/A/a1.milk"));
        tree.set_demo(Some(a1));
        let marked: Vec<String> = tree
            .visible_rows(10)
            .into_iter()
            .filter(|r| r.demo)
            .map(|r| r.label)
            .collect();
        assert_eq!(marked, ["a1"]);
        tree.set_demo(None);
        assert!(tree.visible_rows(10).iter().all(|r| !r.demo));
    }

    #[test]
    fn disabled_round_trip() {
        let mut tree = sample(&DisabledPresets::default());
        tree.toggle(); // A, with everything below it
        tree.move_cursor(1);
        tree.expand();
        tree.move_cursor(1); // b1
        tree.toggle();
        let saved = tree.to_disabled(&DisabledPresets::default());
        // B holds only b1, so it is fully off and is written as a folder.
        assert_eq!(saved.entries().collect::<Vec<_>>(), ["A/", "B/"]);

        let reloaded = sample(&saved);
        assert_eq!(reloaded.counts(), (4, 3));
        assert!(!reloaded.is_enabled(Path::new("/p/A/sub/a2.milk")));
        assert!(!reloaded.is_enabled(Path::new("/p/B/b1.milk")));
        assert!(reloaded.is_enabled(Path::new("/p/top.milk")));
        assert_eq!(reloaded.to_disabled(&saved), saved);
    }

    #[test]
    fn partly_off_folder_lists_its_presets() {
        let (root, list) = paths("/p", &["B/b1.milk", "B/b2.milk"]);
        let mut tree = PresetTree::build(&root, &list, &DisabledPresets::default());
        tree.expand();
        tree.move_cursor(1);
        tree.toggle();
        let saved = tree.to_disabled(&DisabledPresets::default());
        assert_eq!(saved.entries().collect::<Vec<_>>(), ["B/b1.milk"]);
    }

    #[test]
    fn unknown_entries_are_kept_but_ignored() {
        let previous = DisabledPresets::from_lines("Gone/\nB/old.milk\n");
        let tree = sample(&previous);
        assert_eq!(tree.counts(), (4, 0), "nothing in this collection is off");
        let saved = tree.to_disabled(&previous);
        assert_eq!(saved, previous);

        let dir = std::env::temp_dir().join(format!("htpc-tree-{}", std::process::id()));
        let file = dir.join("disabled-presets.txt");
        saved.save(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("Gone/") && text.contains("B/old.milk"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_again_drops_its_line() {
        let previous = DisabledPresets::from_lines("top.milk\nGone/\n");
        let mut tree = sample(&previous);
        tree.move_cursor(2);
        tree.toggle(); // top.milk on again
        let saved = tree.to_disabled(&previous);
        assert_eq!(saved.entries().collect::<Vec<_>>(), ["Gone/"]);
    }

    #[test]
    fn window_follows_cursor() {
        let rels: Vec<String> = (0..30).map(|i| format!("p{i:02}.milk")).collect();
        let rels: Vec<&str> = rels.iter().map(String::as_str).collect();
        let (root, list) = paths("/p", &rels);
        let mut tree = PresetTree::build(&root, &list, &DisabledPresets::default());
        assert_eq!(tree.visible_rows(10).len(), 10);
        tree.move_cursor(1000);
        let rows = tree.visible_rows(10);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows.iter().filter(|r| r.focused).count(), 1);
        assert!(rows.last().unwrap().focused);
        tree.move_cursor(-15);
        let rows = tree.visible_rows(10);
        assert_eq!(rows.iter().filter(|r| r.focused).count(), 1);
        tree.move_cursor(-1000);
        assert!(tree.visible_rows(10)[0].focused);
    }

    #[test]
    fn large_tree_toggle_is_fast() {
        let mut rels = Vec::new();
        for top in 0..5 {
            for sub in 0..40 {
                for n in 0..50 {
                    rels.push(format!("T{top}/S{sub}/p{n}.milk"));
                }
            }
        }
        let rels: Vec<&str> = rels.iter().map(String::as_str).collect();
        let (root, list) = paths("/p", &rels);
        let mut tree = PresetTree::build(&root, &list, &DisabledPresets::default());
        assert_eq!(tree.counts(), (10_000, 0));
        let started = std::time::Instant::now();
        tree.toggle(); // T0, 2,000 presets
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        assert_eq!(tree.counts(), (10_000, 2_000));
    }

    #[test]
    fn empty_tree_is_harmless() {
        let mut tree = PresetTree::build(Path::new("/p"), &[], &DisabledPresets::default());
        tree.move_cursor(1);
        tree.expand();
        tree.collapse();
        tree.toggle();
        assert_eq!(tree.cursor_preset(), None);
        assert!(tree.visible_rows(5).is_empty());
        assert_eq!(tree.counts(), (0, 0));
    }
}
