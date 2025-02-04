use crate::{
    filter::FilterParseError,
    load::{
        get_filename_str, get_ftag_backup_path, get_ftag_path, implicit_tags_str, DirData,
        FileData, FileLoadingOptions, GlobMatches, Loader, LoaderOptions,
    },
    walk::WalkDirectories,
};
use std::{
    ffi::OsStr,
    fmt::Debug,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
};

pub(crate) const FTAG_FILE: &str = ".ftag";
pub(crate) const FTAG_BACKUP_FILE: &str = ".ftagbak";

/// The data related to a glob in an ftag file. This is meant to be used in
/// error reporting.
pub struct GlobInfo {
    glob: String,
    dirpath: PathBuf, // The store file where the glob was found.
}

pub enum Error {
    TUIFailure(String),
    GUIFailure(eframe::Error),
    EditCommandFailed(String),
    UnmatchedGlobs(Vec<GlobInfo>),
    InvalidArgs,
    InvalidWorkingDirectory,
    InvalidPath(PathBuf),
    CannotReadStoreFile(PathBuf),
    CannotParseFtagFile(PathBuf, String),
    CannotWriteFile(PathBuf),
    InvalidFilter(FilterParseError),
    DirectoryTraversalFailed,
}

impl Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TUIFailure(message) => {
                write!(f, "Something went wrong in interactive mode:\n{}", message)
            }
            Self::GUIFailure(e) => write!(f, "Failure in the GUI:\n{}", e),
            Self::EditCommandFailed(message) => write!(f, "Unable to edit file:\n{}", message),
            Self::UnmatchedGlobs(infos) => {
                writeln!(f)?;
                for info in infos {
                    writeln!(
                        f,
                        "No files in '{}' matching '{}'",
                        info.dirpath.display(),
                        info.glob
                    )?;
                }
                Ok(())
            }
            Self::InvalidArgs => write!(f, "Invalid command line arguments"),
            Self::InvalidWorkingDirectory => write!(f, "This is not a valid working directory."),
            Self::InvalidPath(path) => write!(f, "'{}' is not a valid path.", path.display()),
            Self::CannotReadStoreFile(path) => {
                write!(f, "Unable to read file: '{}'", path.display())
            }
            Self::CannotParseFtagFile(path, message) => {
                writeln!(f, "While parsing file '{}'", path.display())?;
                write!(f, "{}", message)
            }
            Self::CannotWriteFile(path) => writeln!(f, "Cannot write to file {}", path.display()),
            Self::InvalidFilter(err) => write!(f, "Unable to parse filter:\n{:?}", err),
            Self::DirectoryTraversalFailed => {
                write!(f, "Something went wrong when traversing directories.")
            }
        }
    }
}

/// Recursively check all directories. This will read all .ftag
/// files, and make sure every listed glob / path matches at least one
/// file on disk.
pub fn check(path: PathBuf) -> Result<(), Error> {
    let mut walker = WalkDirectories::from(path.clone())?;
    let mut matcher = GlobMatches::new();
    let mut loader = Loader::new(LoaderOptions::new(
        false,
        false,
        FileLoadingOptions::Load {
            file_tags: false,
            file_desc: false,
        },
    ));
    let mut missing: Vec<GlobInfo> = Vec::new();
    while let Some((_depth, dirpath, children)) = walker.next() {
        let DirData {
            files,
            desc: _,
            tags: _,
        } = {
            match get_ftag_path::<true>(dirpath) {
                Some(path) => loader.load(&path)?,
                None => continue,
            }
        };
        matcher.find_matches(children, &files, true);
        missing.extend(files.iter().enumerate().filter_map(|(i, f)| {
            if !matcher.is_glob_matched(i) {
                Some(GlobInfo {
                    glob: f.path.to_string(),
                    dirpath: match dirpath.strip_prefix(&path) {
                        Ok(dpath) => dpath.to_path_buf(),
                        Err(_) => dirpath.to_path_buf(),
                    },
                })
            } else {
                None
            }
        }));
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::UnmatchedGlobs(missing))
    }
}

struct FileDataOwned {
    glob: String,
    tags: Vec<String>,
    desc: Option<String>,
}

struct FileDataMultiple {
    globs: Vec<String>,
    tags: Vec<String>,
    desc: Option<String>,
}

fn write_globs<T: AsRef<str>>(globs: &[T], w: &mut impl io::Write) -> Result<(), io::Error> {
    if globs.is_empty() {
        return Ok(());
    }
    writeln!(w, "\n[path]")?;
    for glob in globs.iter().map(|g| g.as_ref()) {
        writeln!(w, "{}", glob)?;
    }
    Ok(())
}

fn write_tags<T: AsRef<str>>(tags: &[T], w: &mut impl io::Write) -> Result<(), io::Error> {
    if tags.is_empty() {
        return Ok(());
    }
    writeln!(w, "[tags]")?;
    if tags
        .iter()
        .try_fold(0usize, |len, tag| -> Result<usize, io::Error> {
            let tag = tag.as_ref();
            Ok(if len > 80 {
                writeln!(w, "{}", tag)?;
                0usize
            } else {
                write!(w, "{} ", tag)?;
                len + tag.len() + 1
            })
        })?
        > 0
    {
        writeln!(w)?;
    }
    Ok(())
}

fn write_desc<T: AsRef<str>>(desc: &Option<T>, w: &mut impl io::Write) -> Result<(), io::Error> {
    if let Some(desc) = desc {
        writeln!(w, "[desc]\n{}", desc.as_ref())
    } else {
        Ok(())
    }
}

pub fn clean(path: PathBuf) -> Result<(), Error> {
    let mut walker = WalkDirectories::from(path.clone())?;
    let mut matcher = GlobMatches::new();
    let mut loader = Loader::new(LoaderOptions::new(
        true,
        true,
        FileLoadingOptions::Load {
            file_tags: true,
            file_desc: true,
        },
    ));
    let mut valid: Vec<FileDataOwned> = Vec::new();
    while let Some((_depth, dirpath, children)) = walker.next() {
        let (path, DirData { files, desc, tags }) = {
            match get_ftag_path::<true>(dirpath) {
                Some(path) => {
                    let dirdata = loader.load(&path)?;
                    (path, dirdata)
                }
                None => continue,
            }
        };
        matcher.find_matches(children, &files, true);
        valid.clear();
        valid.extend(files.iter().enumerate().filter_map(|(i, f)| {
            if matcher.is_glob_matched(i) {
                let mut tags: Vec<String> = f.tags.iter().map(|t| t.to_string()).collect();
                tags.sort();
                tags.dedup();
                Some(FileDataOwned {
                    glob: f.path.to_string(),
                    tags,
                    desc: f.desc.map(|d| d.to_string()),
                })
            } else {
                None
            }
        }));
        // This should group files that share the same tags and desc
        valid.sort_by(|a, b| match a.tags.cmp(&b.tags) {
            std::cmp::Ordering::Less => std::cmp::Ordering::Less,
            std::cmp::Ordering::Equal => a.desc.cmp(&b.desc),
            std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
        });
        // Backup existing data.
        std::fs::copy(&path, get_ftag_backup_path(&path))
            .map_err(|_| Error::CannotWriteFile(path.clone()))?;
        let mut writer = io::BufWriter::new(
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&path)
                .map_err(|_| Error::CannotWriteFile(path.clone()))?,
        );
        // Write directory data.
        write_tags(&tags, &mut writer).map_err(|_| Error::CannotWriteFile(path.clone()))?;
        write_desc(&desc, &mut writer).map_err(|_| Error::CannotWriteFile(path.clone()))?;
        // Write out the file data in groups that share the same tags and description.
        if let Some(last) = valid
            .drain(..)
            .try_fold(
                None,
                |current: Option<FileDataMultiple>,
                 file|
                 -> Result<Option<FileDataMultiple>, io::Error> {
                    Ok(match current {
                        Some(mut current)
                            if current.tags == file.tags && current.desc == file.desc =>
                        {
                            current.globs.push(file.glob);
                            Some(current)
                        }
                        Some(current) => {
                            write_globs(&current.globs, &mut writer)?;
                            write_tags(&current.tags, &mut writer)?;
                            write_desc(&current.desc, &mut writer)?;
                            Some(FileDataMultiple {
                                globs: vec![file.glob],
                                tags: file.tags,
                                desc: file.desc,
                            })
                        }
                        None => Some(FileDataMultiple {
                            globs: vec![file.glob],
                            tags: file.tags,
                            desc: file.desc,
                        }),
                    })
                },
            )
            .map_err(|_| Error::CannotWriteFile(path.clone()))?
        {
            // This is the last entry.
            write_globs(&last.globs, &mut writer)
                .map_err(|_| Error::CannotWriteFile(path.clone()))?;
            write_tags(&last.tags, &mut writer)
                .map_err(|_| Error::CannotWriteFile(path.clone()))?;
            write_desc(&last.desc, &mut writer)
                .map_err(|_| Error::CannotWriteFile(path.clone()))?;
        }
    }
    Ok(())
}

/// Get a description string from the tags and description of a file.
fn full_description(tags: Vec<String>, desc: String) -> String {
    let tagstr = {
        let mut tags = tags.into_iter();
        let first = tags.next().unwrap_or_default();
        tags.fold(first, |acc, t| format!("{}, {}", acc, t))
    };
    format!(
        "tags: [{}]{}",
        tagstr,
        if desc.is_empty() {
            desc
        } else {
            format!("\n{}", desc)
        }
    )
}

/// Get the description of a file or a directory.
pub fn what_is(path: &Path) -> Result<String, Error> {
    if path.is_file() {
        what_is_file(path)
    } else if path.is_dir() {
        what_is_dir(path)
    } else {
        Err(Error::InvalidPath(path.to_path_buf()))
    }
}

/// Get a full description of the file that includes the tags and the
/// description of said file.
fn what_is_file(path: &Path) -> Result<String, Error> {
    use fast_glob::glob_match;
    let mut loader = Loader::new(LoaderOptions::new(
        true,
        true,
        FileLoadingOptions::Load {
            file_tags: true,
            file_desc: true,
        },
    ));
    let DirData { desc, tags, files } = {
        match get_ftag_path::<true>(path) {
            Some(storepath) => loader.load(&storepath)?,
            None => return Err(Error::InvalidPath(path.to_path_buf())),
        }
    };
    let mut outdesc = desc.unwrap_or("").to_string();
    let mut outtags = tags.iter().map(|t| t.to_string()).collect::<Vec<_>>();
    if let Some(parent) = path.parent() {
        outtags.extend(implicit_tags_str(get_filename_str(parent)?));
    }
    let filenamestr = path
        .file_name()
        .ok_or(Error::InvalidPath(path.to_path_buf()))?
        .to_str()
        .ok_or(Error::InvalidPath(path.to_path_buf()))?;
    for FileData {
        path: pattern,
        desc: fdesc,
        tags: ftags,
    } in files
    {
        if glob_match(pattern, filenamestr) {
            outtags.extend(
                ftags
                    .iter()
                    .map(|t| t.to_string())
                    .chain(implicit_tags_str(filenamestr)),
            );
            if let Some(fdesc) = fdesc {
                outdesc = format!("{}\n{}", fdesc, outdesc);
            }
        }
    }
    // Remove duplicate tags.
    outtags.sort();
    outtags.dedup();
    Ok(full_description(outtags, outdesc))
}

/// Get the full description of a directory that includes it's tags and
/// description.
fn what_is_dir(path: &Path) -> Result<String, Error> {
    let mut loader = Loader::new(LoaderOptions::new(true, true, FileLoadingOptions::Skip));
    let DirData {
        desc,
        tags,
        files: _,
    } = {
        match get_ftag_path::<true>(path) {
            Some(storepath) => loader.load(&storepath)?,
            None => return Err(Error::InvalidPath(path.to_path_buf())),
        }
    };
    let desc = desc.unwrap_or("").to_string();
    let tags = tags
        .iter()
        .map(|t| t.to_string())
        .chain(implicit_tags_str(get_filename_str(path)?))
        .collect::<Vec<_>>();
    Ok(full_description(tags, desc))
}

/// Get the path of `filename` which is in the directory `dirpath`, relative to `root`.
pub(crate) fn get_relative_path(dirpath: &Path, filename: &OsStr, root: &Path) -> Option<PathBuf> {
    match dirpath.strip_prefix(root) {
        Ok(path) => {
            let mut path = PathBuf::from(path);
            path.push(filename);
            Some(path)
        }
        Err(_) => None,
    }
}

/// Recursively traverse the directories starting from `root` and
/// return all files that are not tracked.
pub fn untracked_files(root: PathBuf) -> Result<Vec<PathBuf>, Error> {
    use fast_glob::glob_match;
    let mut walker = WalkDirectories::from(root.clone())?;
    let mut untracked: Vec<PathBuf> = Vec::new();
    let mut loader = Loader::new(LoaderOptions::new(
        false,
        false,
        FileLoadingOptions::Load {
            file_tags: false,
            file_desc: false,
        },
    ));
    while let Some((_depth, dirpath, children)) = walker.next() {
        let DirData {
            files: patterns,
            desc: _,
            tags: _,
        }: DirData = {
            match get_ftag_path::<true>(dirpath) {
                Some(path) => loader.load(&path)?,
                // Store file doesn't exist so everything is untracked.
                None => {
                    untracked.extend(
                        children
                            .iter()
                            .filter_map(|f| get_relative_path(dirpath, f.name(), &root)),
                    );
                    continue;
                }
            }
        };
        untracked.extend(children.iter().filter_map(|child| {
            let fnamestr = child.name().to_str()?;
            if patterns.iter().any(|p| glob_match(p.path, fnamestr)) {
                None
            } else {
                get_relative_path(dirpath, child.name(), &root)
            }
        }));
    }
    Ok(untracked)
}

/// Recursively traverse the directories from `path` and get all tags.
pub fn get_all_tags(path: PathBuf) -> Result<Vec<String>, Error> {
    let mut alltags: Vec<String> = Vec::new();
    let mut walker = WalkDirectories::from(path)?;
    let mut loader = Loader::new(LoaderOptions::new(
        true,
        false,
        FileLoadingOptions::Load {
            file_tags: true,
            file_desc: false,
        },
    ));
    while let Some((_depth, dirpath, _filenames)) = walker.next() {
        let DirData {
            mut tags,
            mut files,
            desc: _,
        } = {
            match get_ftag_path::<true>(dirpath) {
                Some(path) => loader.load(&path)?,
                None => continue,
            }
        };
        alltags.extend(
            tags.drain(..)
                .map(|t| t.to_string())
                .chain(implicit_tags_str(get_filename_str(dirpath)?)),
        );
        for mut fdata in files.drain(..) {
            alltags.extend(
                fdata
                    .tags
                    .drain(..)
                    .map(|t| t.to_string())
                    .chain(implicit_tags_str(fdata.path)),
            );
        }
    }
    alltags.sort();
    alltags.dedup();
    Ok(alltags)
}

pub fn search(path: PathBuf, needle: &str) -> Result<(), Error> {
    let words: Vec<_> = needle
        .trim()
        .split(|c: char| !c.is_alphanumeric())
        .map(|word| word.trim().to_lowercase())
        .collect();
    let mut walker = WalkDirectories::from(path)?;
    let mut loader = Loader::new(LoaderOptions::new(
        true,
        true,
        FileLoadingOptions::Load {
            file_tags: true,
            file_desc: true,
        },
    ));
    let match_fn = |tags: &[&str], desc: Option<&str>| {
        tags.iter().any(|tag| {
            // Check if tag matches
            let lower = tag.to_lowercase();
            words
                .iter()
                .any(|word| lower.matches(word).next().is_some())
        }) || match desc {
            // Check if description matches.
            Some(desc) => {
                let desc = desc.to_lowercase();
                words.iter().any(|word| desc.matches(word).next().is_some())
            }
            None => false,
        }
    };
    while let Some((_depth, dirpath, _filenames)) = walker.next() {
        let DirData { tags, files, desc } = {
            match get_ftag_path::<true>(dirpath) {
                Some(path) => loader.load(&path)?,
                None => continue,
            }
        };
        let dirmatch = match_fn(&tags, desc);
        for filepath in files.iter().filter_map(|f| {
            if dirmatch || match_fn(&f.tags, f.desc) {
                Some(f.path)
            } else {
                None
            }
        }) {
            println!("{}", filepath);
        }
    }
    Ok(())
}
