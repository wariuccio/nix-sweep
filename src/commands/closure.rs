use std::fs;
use std::path::PathBuf;

use colored::Colorize;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::slice::ParallelSliceMut;

use crate::utils::fmt::*;
use crate::nix::store::StorePath;


#[derive(clap::Args)]
pub struct ClosureCommand {
    /// Path to closure
    path: PathBuf,

    /// Sort by path size instead of closure size
    #[clap(long)]
    by_path_size: bool,

    /// Reverse sorting
    #[clap(short, long)]
    reverse: bool,

    /// Show hashes
    #[clap(short('H'), long)]
    hash: bool,
}

impl super::Command for ClosureCommand {
    fn run(self) -> Result<(), String> {
        let root = StorePath::from_symlink(&self.path)?;
        let root_closure = root.closure()?;
        let mut root_closure_info: Vec<_> = root_closure.into_par_iter()
            .map(|p| (p.name(), p.hash(), p.size_naive(), p.closure_size_naive()))
            .collect();

        if self.by_path_size {
            root_closure_info.par_sort_by_key(|(_, _, path_size, _)| *path_size);
        } else {
            root_closure_info.par_sort_by_key(|(_, _, _, closure_size)| *closure_size);
        }
        if !self.reverse {
            root_closure_info.reverse();
        }

        let longest_name_len = root_closure_info.iter()
            .map(|(n, h, _, _)| if self.hash { h.len() + n.len() + 1 } else { n.len() })
            .max()
            .unwrap_or_default();

        let escape_offset = if colored::control::SHOULD_COLORIZE.should_colorize() {
            9
        } else {
            0
        };

        for (name, hash, size, closure_size) in root_closure_info {
            let full_name = if self.hash {
                format!("{}-{}", hash, name.bright_blue())
            } else {
                name.bright_blue().to_string()
            };

            println!("{:<align_name$} {} / {}",
                full_name,
                FmtSize::new(size).to_string().yellow(),
                FmtSize::new(closure_size).to_string().yellow(),
                align_name=longest_name_len + escape_offset);
        }



        Ok(())
    }
}
