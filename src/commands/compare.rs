use colored::Colorize;
use std::path::PathBuf;
use std::sync::Mutex;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::slice::ParallelSliceMut;

use crate::nix::store::StorePath;
use crate::utils::fmt::{FmtSize, FmtWithEllipsis, Formattable};
use crate::utils::interaction;


const SHORT_HASH_LEN: usize = 7;


#[derive(clap::Args)]
pub struct CompareCommand {
    /// Baseline path to compare to
    baseline: PathBuf,

    /// Next comparison path
    current: PathBuf,

    /// Mention paths only once per name + version
    #[clap(short, long)]
    concise: bool,

    /// Do not show added paths
    #[clap(long)]
    no_added: bool,

    /// Do not show removed paths
    #[clap(long)]
    no_removed: bool,

    /// Do not show updated paths
    #[clap(long)]
    no_updated: bool,

    /// Do not show changed paths
    #[clap(long)]
    no_changed: bool,

    /// Do not show a summary at the end
    #[clap(long)]
    no_summary: bool,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct PathItem {
    hash: String,
    name: String,
    version: Option<String>,
}


impl TryFrom<&StorePath> for PathItem {
    type Error = String;

    fn try_from(value: &StorePath) -> Result<Self, Self::Error> {
        let value = value.path();

        let filename = value.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| String::from("Unable to parse store path"))?;

        let (hash, rest) = filename.split_once('-')
            .ok_or_else(|| String::from("Unable to parse store path"))?;
        let hash = hash.to_owned();

        if !hash.chars().all(|c| c.is_ascii_alphanumeric() && (c.is_lowercase() || c.is_numeric())) {
            return Err(String::from("Unable to parse store path"));
        }

        let tokens: Vec<_> = rest.split('-').collect();
        let name = tokens.iter()
            .take_while(|t| t.chars().next().map(|c| c.is_alphabetic()).unwrap_or_default())
            .map(|s| *s)
            .collect::<Vec<_>>()
            .join("-")
            .to_owned();
        let remaining = tokens.iter()
            .skip_while(|t| t.chars().next().map(|c| c.is_alphabetic()).unwrap_or_default())
            .map(|s| *s)
            .collect::<Vec<_>>();

        let version = if remaining.is_empty() {
            None
        } else {
            Some(remaining.join("-"))
        };

        Ok(PathItem { hash, name, version })
    }
}

impl super::Command for CompareCommand {
    fn run(self) -> Result<(), String> {
        let baseline = StorePath::new(self.baseline.canonicalize().map_err(|e| e.to_string())?)?;
        let current = StorePath::new(self.current.canonicalize().map_err(|e| e.to_string())?)?;

        // Fetch closure of both paths and convert to `PathItem`
        let baseline_closure = baseline.closure()?;
        let current_closure = current.closure()?;
        let baseline_size = baseline.closure_size();
        let current_size = current.closure_size();
        let baseline_items: Vec<_> = baseline_closure.into_iter()
            .map(|s| PathItem::try_from(&s))
            .collect::<Result<_, _>>()?;
        let current_items: Vec<_> = current_closure.into_iter()
            .map(|s| PathItem::try_from(&s))
            .collect::<Result<_, _>>()?;

        let version_changed = Mutex::new(Vec::new());
        let hash_changed = Mutex::new(Vec::new());
        let added = Mutex::new(Vec::new());
        let removed = Mutex::new(Vec::new());

        // Find candidates for changed and added packages
        current_items.par_iter()
            .filter(|ci| !baseline_items.iter().any(|bi| *ci == bi))
            .for_each(|ci| {
                let mut found_match = false;

                baseline_items.iter().for_each(|bi| {
                    if ci.name == bi.name {
                        found_match = true;
                        if ci.version == bi.version {
                            if ci.hash != bi.hash {
                                hash_changed.lock().unwrap().push((bi.clone(), ci.clone()));
                            }
                        } else {
                            version_changed.lock().unwrap().push((bi.clone(), ci.clone()));
                        }
                    }
                });

                if !found_match {
                    added.lock().unwrap().push(ci.clone());
                }
            });

        // Find candidates for removed packages
        baseline_items.par_iter()
            .for_each(|bi| if !current_items.iter().any(|ci| bi.name == ci.name) {
                removed.lock().unwrap().push(bi.clone());
            });

        let mut version_changed = version_changed.into_inner().unwrap();
        let mut hash_changed = hash_changed.into_inner().unwrap();
        let mut added = added.into_inner().unwrap();
        let mut removed = removed.into_inner().unwrap();

        // Filter out mismatches and duplicates
        let different_version = |a: &PathItem, b: &PathItem| a.name != b.name || a.version != b.version;
        version_changed.retain(|(bi, ci)| baseline_items.par_iter().all(|bi| different_version(bi, ci))
            && current_items.par_iter().all(|ci| different_version(bi, ci)));
        hash_changed.retain(|(bi, ci)| baseline_items.par_iter().all(|bi| ci != bi)
            && current_items.par_iter().all(|ci| bi != ci));

        // Sort lists
        version_changed.par_sort_by(|a, b| a.1.version.cmp(&b.1.version));
        version_changed.par_sort_by(|a, b| a.0.version.cmp(&b.0.version));
        version_changed.par_sort_by(|a, b| a.0.name.cmp(&b.0.name));
        hash_changed.par_sort_by(|a, b| a.1.version.cmp(&b.1.version));
        hash_changed.par_sort_by(|a, b| a.0.version.cmp(&b.0.version));
        hash_changed.par_sort_by(|a, b| a.0.name.cmp(&b.0.name));
        added.par_sort_by(|a, b| a.version.cmp(&b.version));
        added.par_sort_by(|a, b| a.name.cmp(&b.name));
        removed.par_sort_by(|a, b| a.version.cmp(&b.version));
        removed.par_sort_by(|a, b| a.name.cmp(&b.name));

        // Remove items only differing by hash in simplified mode
        if self.concise {
            let either_is_prefix  = |a: &str, b: &str| a.starts_with(b) || b.starts_with(a);
            version_changed.dedup_by(|a, b| a.0.name == b.0.name && either_is_prefix(&a.0.name, &b.0.name));
            hash_changed.dedup_by(|a, b| a.0.name == b.0.name && either_is_prefix(&a.0.name, &b.0.name));
            added.dedup_by(|a, b| a.name == b.name && either_is_prefix(&a.name, &b.name));
            removed.dedup_by(|a, b| a.name == b.name && either_is_prefix(&a.name, &b.name));
        }

        if !self.no_added && !added.is_empty() {
            print_added(&added, self.concise);
        }

        if !self.no_removed && !removed.is_empty() {
            print_removed(&removed, self.concise);
        }

        if !self.no_updated && !version_changed.is_empty() {
            print_updated(&version_changed, self.concise);
        }

        if !self.no_changed && !hash_changed.is_empty() {
            print_changed(&hash_changed, self.concise);
        }

        if !self.no_summary {
            print_summary(&self, baseline_size, current_size, baseline_items.len(), current_items.len());
        }

        Ok(())
    }
}

fn version_len(p: &PathItem) -> usize {
    p.version.as_ref().map(|v| v.len()).unwrap_or_default()
}

fn version_len_both((p, q): &(PathItem, PathItem)) -> usize {
    version_len(p).max(version_len(q))
}

fn print_added(added: &[PathItem], concise: bool) {
    interaction::announce(&format!("{} packages added:", added.len()));
    let max_name_len = added.iter().map(|p| p.name.len()).max().unwrap_or_default();
    let max_version_len = added.iter().map(version_len).max().unwrap_or_default();
    for ci in added {
        let name = ci.name.clone();
        let version = ci.version.clone().unwrap_or_default();
        let reserved_space = if concise { max_version_len + 4 } else { max_version_len + 14 };
        let fmt_name = FmtWithEllipsis::fitting_terminal(name, max_name_len, reserved_space)
            .right_pad()
            .to_string()
            .blue();
        let fmt_version = FmtWithEllipsis::fitting_terminal(version, max_version_len, 0)
            .left_pad()
            .to_string()
            .green();
        if concise {
            println!("  {}  {}", fmt_name, fmt_version);
        } else {
            println!("  {}  {} ({})", fmt_name, fmt_version, &ci.hash[..SHORT_HASH_LEN]);
        }
    }
}

fn print_removed(removed: &[PathItem], concise: bool) {
    interaction::announce(&format!("{} packages removed:", removed.len()));
    let max_name_len = removed.iter().map(|p| p.name.len()).max().unwrap_or_default();
    let max_version_len = removed.iter().map(version_len).max().unwrap_or_default();
    for bi in removed {
        let name = bi.name.clone();
        let version = bi.version.clone().unwrap_or_default();
        let reserved_space = if concise { max_version_len + 4 } else { max_version_len + 14 };
        let fmt_name = FmtWithEllipsis::fitting_terminal(name, max_name_len, reserved_space)
            .right_pad()
            .to_string()
            .blue();
        let fmt_version = FmtWithEllipsis::fitting_terminal(version, max_version_len, 0)
            .left_pad()
            .to_string()
            .red();
        if concise {
            println!("  {}  {}", fmt_name, fmt_version);
        } else {
            println!("  {}  {} ({})", fmt_name, fmt_version, &bi.hash[..SHORT_HASH_LEN]);
        }
    }
}

fn print_updated(version_changed: &[(PathItem, PathItem)], concise: bool) {
    interaction::announce(&format!("{} packages updated:", version_changed.len()));
    let max_name_len = version_changed.iter().map(|(p, _)| p.name.len()).max().unwrap_or_default();
    let max_version_len = version_changed.iter().map(version_len_both).max().unwrap_or_default();
    for (bi, ci) in version_changed {
        let name = bi.name.clone();
        let baseline_version = bi.version.clone().unwrap_or_default();
        let current_version = ci.version.clone().unwrap_or_default();
        let reserved_space = if concise { 2*max_version_len + 10 } else { 2*max_version_len + 30 };
        let fmt_name = FmtWithEllipsis::fitting_terminal(name, max_name_len, reserved_space)
            .right_pad()
            .to_string()
            .blue();
        let fmt_baseline_version = FmtWithEllipsis::fitting_terminal(baseline_version, max_version_len, 0)
            .left_pad()
            .to_string()
            .red();
        let fmt_current_version = FmtWithEllipsis::fitting_terminal(current_version, max_version_len, 0)
            .left_pad()
            .to_string()
            .green();
        if concise {
            println!("  {}  {}  ->  {}", fmt_name, fmt_baseline_version, fmt_current_version);
        } else {
            println!("  {}  {} ({})  ->  {} ({})",
            fmt_name,
            fmt_baseline_version, &bi.hash[..SHORT_HASH_LEN],
            fmt_current_version, &ci.hash[..SHORT_HASH_LEN]);
        }
    }

}

fn print_changed(hash_changed: &[(PathItem, PathItem)], concise: bool) {
    interaction::announce(&format!("{} packages changed:", hash_changed.len()));
    let max_name_len = hash_changed.iter().map(|(p, _)| p.name.len()).max().unwrap_or_default();
    let max_version_len = hash_changed.iter().map(version_len_both).max().unwrap_or_default() + 2;
    for (bi, ci) in hash_changed {
        let name = bi.name.clone();
        let version = bi.version.as_ref().map(|v| format!("({})", v)).unwrap_or_default();
        let baseline_hash = &bi.hash[..SHORT_HASH_LEN];
        let current_hash = &ci.hash[..SHORT_HASH_LEN];
        let reserved_space = if concise { 4 + max_version_len } else { 2*SHORT_HASH_LEN + max_version_len + 12 };
        let fmt_name = FmtWithEllipsis::fitting_terminal(name, max_name_len, reserved_space)
            .right_pad()
            .to_string()
            .blue();
        let fmt_baseline_hash = FmtWithEllipsis::fitting_terminal(baseline_hash.to_owned(), SHORT_HASH_LEN, 0).left_pad()
            .to_string()
            .red();
        let fmt_current_hash = FmtWithEllipsis::fitting_terminal(current_hash.to_owned(), SHORT_HASH_LEN, 0)
            .left_pad()
            .to_string()
            .green();
        let fmt_version = FmtWithEllipsis::fitting_terminal(version.to_owned(), max_version_len, 0)
            .left_pad();
        if concise {
            println!("  {}  {}", fmt_name, fmt_version);
        } else {
            println!("  {}  {}  ->  {}  {}", fmt_name, fmt_baseline_hash, fmt_current_hash, fmt_version);
        }
    }

}


fn print_summary(command: &CompareCommand, baseline_size: u64, current_size: u64, baseline_nitems: usize, current_nitems: usize) {
    interaction::announce("Summary");
    let net_diff_label = "net difference";
    let size_diff = current_size as i64 - baseline_size as i64;
    let paths_diff = current_nitems as i64 - baseline_nitems as i64;
    let baseline_path_str = command.baseline.to_string_lossy().to_string();
    let current_path_str = command.current.to_string_lossy().to_string();
    let max_path_len = baseline_path_str.len().max(current_path_str.len()).max(net_diff_label.len());
    let max_npaths_len = baseline_nitems.to_string().len().max(current_nitems.to_string().len());
    let reserved_space = FmtSize::MAX_WIDTH + max_npaths_len + 14;
    let fmt_baseline_path = FmtWithEllipsis::fitting_terminal(baseline_path_str, max_path_len, reserved_space)
        .left_pad()
        .to_string()
        .bright_blue();
    let fmt_current_path = FmtWithEllipsis::fitting_terminal(current_path_str, max_path_len, reserved_space)
        .left_pad()
        .to_string()
        .bright_blue();
    let fmt_diff_path = FmtWithEllipsis::fitting_terminal(net_diff_label.to_string(), max_path_len, reserved_space)
        .left_pad()
        .to_string();
    let fmt_baseline_size = FmtSize::new(baseline_size)
        .left_pad()
        .to_string()
        .yellow();
    let fmt_current_size = FmtSize::new(current_size)
        .left_pad()
        .to_string()
        .yellow();
    let fmt_diff_size = if size_diff == 0 {
        FmtSize::new(size_diff.unsigned_abs())
            .left_pad()
            .to_string()
            .into()
    } else if size_diff < 0 {
        FmtSize::new(size_diff.unsigned_abs())
            .with_prefix::<1>("-".to_string())
            .left_pad()
            .to_string()
            .green()
    } else {
        FmtSize::new(size_diff.unsigned_abs())
            .with_prefix::<1>("+".to_string())
            .left_pad()
            .to_string()
            .red()
    };
    let fmt_baseline_paths = FmtWithEllipsis::fitting_terminal(baseline_nitems.to_string(), max_npaths_len, 0);
    let fmt_current_paths = FmtWithEllipsis::fitting_terminal(current_nitems.to_string(), max_npaths_len, 0);
    let fmt_diff_paths = if paths_diff == 0 {
        format!("{}", paths_diff.unsigned_abs())
            .into()
    } else if paths_diff < 0 {
        format!("-{}", paths_diff.unsigned_abs())
            .green()
    } else {
        format!("+{}", paths_diff.unsigned_abs())
            .red()
    };

    println!("{}:  {} ({} paths)",
    fmt_baseline_path,
    fmt_baseline_size,
    fmt_baseline_paths,
    );
    println!("{}:  {} ({} paths)",
    fmt_current_path,
    fmt_current_size,
    fmt_current_paths,
    );
    println!("{}: {} ({} paths)",
    fmt_diff_path,
    fmt_diff_size,
    fmt_diff_paths,
    );
}
