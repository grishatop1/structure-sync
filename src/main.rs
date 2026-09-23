use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use clap::Parser;
use walkdir::{DirEntry, WalkDir};

type FileTable = HashMap<(OsString, u64), DirEntry>;

#[derive(Default)]
struct Actions {
    required_dir_creations: HashMap<PathBuf, u64>,
    required_file_moves: Vec<(PathBuf, PathBuf)>,
    required_copies: Vec<(PathBuf, PathBuf)>,
    deletions: Vec<PathBuf>,
}

/// Sync directory structures between two paths.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// First directory path (original structure)
    #[arg(value_parser = parse_directory, value_hint = clap::ValueHint::DirPath)]
    path1: PathBuf,

    /// Second directory path (structure to modify)
    #[arg(value_parser = parse_directory, value_hint = clap::ValueHint::DirPath)]
    path2: PathBuf,

    /// WARNING: will perform actions on the target directory
    #[arg(long)]
    action: bool,

    /// Instead of syncing structure, only fix modification times: for every
    /// file that exists in both path1 and path2,
    /// set path2's mtime to match path1's.
    #[arg(long)]
    mtime_fix: bool,
}

fn parse_directory(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);

    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }

    if !path.is_dir() {
        return Err(format!("path is not a directory: {}", path.display()));
    }

    Ok(path)
}

fn main() {
    let cli = Cli::parse();

    let table1 = create_file_table(&cli.path1);
    let table2 = create_file_table(&cli.path2);

    if cli.mtime_fix {
        run_mtime_fix(table1, table2, cli.action);
        return;
    }

    let actions = compare_two_tables(table1, table2, cli.path1, cli.path2);

    if !&cli.action {
        for a in &actions.required_file_moves {
            println!("move: {} -> {}", a.0.display(), a.1.display());
        }
        for (dir, count) in &actions.required_dir_creations {
            println!("create: {} *{} moves*", dir.display(), count);
        }
        for dir in &actions.deletions {
            println!("delete: {}", dir.display());
        }
        for (from, to) in &actions.required_copies {
            println!("copy: {} -> {}", from.display(), to.display());
        }

        if actions.required_dir_creations.is_empty()
            && actions.required_copies.is_empty()
            && actions.deletions.is_empty()
            && actions.required_file_moves.is_empty()
        {
            println!("directories are in sync! :)");
            return;
        }
    } else {
        println!("performing actions...");

        for dir in &actions.required_dir_creations {
            if let Err(e) = std::fs::create_dir_all(dir.0) {
                eprintln!("failed to create dir {}: {}", dir.0.display(), e);
            }
        }
        for (from, to) in &actions.required_file_moves {
            if let Err(e) = std::fs::rename(from, to) {
                eprintln!(
                    "failed to move {} -> {}: {}",
                    from.display(),
                    to.display(),
                    e
                );
            }
        }
        for path in &actions.deletions {
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            if let Err(e) = result {
                eprintln!("failed to delete {}: {}", path.display(), e);
            }
        }
        for (from, to) in &actions.required_copies {
            if let Err(e) = std::fs::copy(from, to) {
                eprintln!(
                    "failed to copy {} -> {}: {}",
                    from.display(),
                    to.display(),
                    e
                );
            }
            match std::fs::metadata(from).and_then(|m| m.modified()) {
                Ok(mtime) => {
                    let ft = filetime::FileTime::from_system_time(mtime);
                    if let Err(e) = filetime::set_file_mtime(to, ft) {
                        eprintln!(
                            "copied {} but failed to set mtime on {}: {}",
                            from.display(),
                            to.display(),
                            e
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "copied {} but failed to read source mtime: {}",
                        from.display(),
                        e
                    );
                }
            }
        }
    }
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with("."))
        .unwrap_or(false)
}

fn create_file_table(path: &PathBuf) -> FileTable {
    let mut table = HashMap::new();

    let walker = WalkDir::new(path).into_iter();
    for entry in walker
        .filter_entry(|e| !is_hidden(e))
        .filter_map(|e| e.ok())
    {
        let filename = entry.file_name().to_owned();
        let size = entry.metadata().map(|m| m.len()).unwrap_or_default();
        if table.contains_key(&(filename.clone(), size)) {
            println!("duplicate found: {}", entry.path().to_string_lossy());
            continue;
        }
        table.insert((filename, size), entry);
    }

    table
}

fn compare_two_tables(t1: FileTable, t2: FileTable, t1_path: PathBuf, t2_path: PathBuf) -> Actions {
    let mut actions = Actions::default();

    for f1 in &t1 {
        // skip if dir
        if f1.1.file_type().is_dir() {
            continue;
        }

        let original_entry = &f1.1;
        let original_relative = get_relative_path(&t1_path, original_entry.path());

        // if the target path does not exist, add it to the required dir creations
        let target_dir_creation = t2_path.join(&original_relative.parent().unwrap());
        if !target_dir_creation.exists() {
            actions
                .required_dir_creations
                .entry(target_dir_creation.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }

        let Some(target_entry) = t2.get(&f1.0) else {
            // println!("file not found in target path: {}", f1.0.0.display());

            actions
                .required_copies
                .push((f1.1.path().to_owned(), t2_path.join(&original_relative)));
            continue;
        };

        let target_destination = target_dir_creation.join(original_relative.file_name().unwrap());

        if !target_destination.exists() {
            actions
                .required_file_moves
                .push((target_entry.path().to_owned(), target_destination));
        }
    }

    for f2 in &t2 {
        if !t1.contains_key(&f2.0) {
            let target_entry = &f2.1;
            let target_relative = get_relative_path(&t2_path, target_entry.path());
            let original_check_path = t1_path.join(&target_relative);

            if !original_check_path.exists() {
                actions.deletions.push(target_entry.path().to_owned());
            }
        }
    }
    actions
}

fn get_relative_path(root_path: &PathBuf, file_path: &Path) -> PathBuf {
    file_path
        .strip_prefix(root_path)
        .unwrap_or(file_path)
        .to_path_buf()
}

fn run_mtime_fix(t1: FileTable, t2: FileTable, apply: bool) {
    let mut fixed = 0u64;
    let mut already_matched = 0u64;
    let mut missing_on_target = 0u64;
    let mut errors = 0u64;

    for (key, entry1) in &t1 {
        if entry1.file_type().is_dir() {
            continue;
        }

        let Some(entry2) = t2.get(key) else {
            println!("not found on target: {}", entry1.path().display());
            missing_on_target += 1;
            continue;
        };

        let metadata1 = match entry1.metadata() {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "failed to read metadata for {}: {}",
                    entry1.path().display(),
                    e
                );
                errors += 1;
                continue;
            }
        };

        let mtime1 = match metadata1.modified() {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "failed to read mtime for {}: {}",
                    entry1.path().display(),
                    e
                );
                errors += 1;
                continue;
            }
        };

        let mtime2 = entry2.metadata().ok().and_then(|m| m.modified().ok());

        if mtime2 == Some(mtime1) {
            already_matched += 1;
            continue;
        }

        if apply {
            let ft = filetime::FileTime::from_system_time(mtime1);
            if let Err(e) = filetime::set_file_mtime(entry2.path(), ft) {
                eprintln!("failed to set mtime on {}: {}", entry2.path().display(), e);
                errors += 1;
                continue;
            }
            println!("fixed: {}", entry2.path().display());
        } else {
            println!(
                "would fix: {} (currently {:?}, would become {:?})",
                entry2.path().display(),
                mtime2,
                mtime1
            );
        }
        fixed += 1;
    }

    println!(
        "\n{} {}, {} already matched, {} missing on target, {} errors",
        if apply { "fixed" } else { "would fix" },
        fixed,
        already_matched,
        missing_on_target,
        errors
    );

    if !apply && fixed > 0 {
        println!("(dry run! pass --action to actually apply these changes)");
    }
}
