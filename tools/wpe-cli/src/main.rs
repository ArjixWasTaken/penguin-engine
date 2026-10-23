mod scenes;
mod tex;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use wpe_vfs::PkgFs;

#[derive(Parser)]
#[command(about = "Wallpaper Engine asset inspection tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the entries of a .pkg archive.
    Ls { pkg: PathBuf },
    /// Extract every entry of a .pkg archive into a directory.
    Extract { pkg: PathBuf, out: PathBuf },
    /// Parse every scene.pkg under a workshop content directory and report failures.
    ScanPkgs { dir: PathBuf },
    /// Decode the first mip of a .tex to PNG. With --pkg, INPUT is a path inside that archive.
    Tex2png {
        input: String,
        out: PathBuf,
        #[arg(long)]
        pkg: Option<PathBuf>,
    },
    /// Decode every .tex in workshop scene.pkg files and loose asset directories.
    ScanTex {
        /// Workshop content directory (containing <id>/scene.pkg).
        #[arg(long)]
        workshop: Option<PathBuf>,
        /// Loose asset directories (e.g. Wallpaper Engine's assets/).
        #[arg(long)]
        assets: Vec<PathBuf>,
    },
    /// Parse project, scene, model, material and effect JSON of every workshop scene.
    ScanScenes {
        /// Wallpaper Engine's stock assets/ directory.
        #[arg(long)]
        assets: PathBuf,
        /// Workshop content directory (containing <id>/scene.pkg).
        #[arg(long)]
        workshop: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Ls { pkg } => ls(&pkg),
        Command::Extract { pkg, out } => extract(&pkg, &out),
        Command::ScanPkgs { dir } => scan_pkgs(&dir),
        Command::Tex2png { input, out, pkg } => tex::tex2png(&input, &out, pkg.as_deref()),
        Command::ScanTex { workshop, assets } => tex::scan_tex(workshop.as_deref(), &assets),
        Command::ScanScenes { assets, workshop } => scenes::scan_scenes(&assets, &workshop),
    }
}

fn ls(path: &Path) -> Result<()> {
    let pkg = PkgFs::open(path)?;
    println!("{} ({} entries)", pkg.version(), pkg.entries().len());
    for e in pkg.entries() {
        println!("{:>10}  {}", e.length, e.path);
    }
    Ok(())
}

fn extract(path: &Path, out: &Path) -> Result<()> {
    let pkg = PkgFs::open(path)?;
    for e in pkg.entries() {
        let rel = Path::new(&e.path);
        anyhow::ensure!(
            rel.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "refusing unsafe entry path {:?}",
            e.path
        );
        let dest = out.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, pkg.entry_bytes(e))
            .with_context(|| format!("writing {}", dest.display()))?;
    }
    println!(
        "extracted {} entries to {}",
        pkg.entries().len(),
        out.display()
    );
    Ok(())
}

fn scan_pkgs(dir: &Path) -> Result<()> {
    let (mut ok, mut failed, mut entries) = (0usize, 0usize, 0usize);
    let mut versions = std::collections::BTreeMap::<String, usize>::new();
    for item in std::fs::read_dir(dir)? {
        let pkg_path = item?.path().join("scene.pkg");
        if !pkg_path.exists() {
            continue;
        }
        match PkgFs::open(&pkg_path) {
            Ok(pkg) => {
                ok += 1;
                entries += pkg.entries().len();
                *versions.entry(pkg.version().to_owned()).or_default() += 1;
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAIL {}: {e}", pkg_path.display());
            }
        }
    }
    println!("{ok} ok, {failed} failed, {entries} entries total");
    for (v, n) in versions {
        println!("  {v}: {n}");
    }
    Ok(())
}
