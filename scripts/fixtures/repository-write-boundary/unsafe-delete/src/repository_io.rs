pub(crate) fn unauthenticated_repository_deleter(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

pub(crate) fn unauthenticated_aliased_repository_deleter(
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::fs::{
        remove_file as erase,
    };

    erase(path)
}

pub(crate) fn unauthenticated_aliased_repository_unlinkat(
    directory: impl std::os::fd::AsFd,
    name: &std::ffi::OsStr,
) -> rustix::io::Result<()> {
    use rustix::fs as repository_fs;

    repository_fs::unlinkat(directory, name, repository_fs::AtFlags::empty())
}

pub(crate) fn unauthenticated_imported_repository_deleter(
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::fs::{
        OpenOptions,
        remove_file,
    };

    let _ = OpenOptions::new().write(true).open(path)?;
    remove_file(path)
}

pub(crate) fn unauthenticated_glob_imported_repository_deleter(
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::fs::*;

    remove_file(path)
}

pub(crate) fn unauthenticated_repository_directory_deleter(
    empty: &std::path::Path,
    recursive: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::remove_dir(empty)?;
    std::fs::remove_dir_all(recursive)
}

pub(crate) fn unauthenticated_repository_directory_creator(
    directory: impl std::os::fd::AsFd,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    std::fs::create_dir(path)?;
    std::fs::create_dir_all(path)?;
    rustix::fs::mkdirat(directory, "child", rustix::fs::Mode::empty())?;
    Ok(())
}

pub(crate) fn unauthenticated_repository_unlinkat(
    directory: impl std::os::fd::AsFd,
    name: &std::ffi::OsStr,
) -> rustix::io::Result<()> {
    rustix::fs::unlinkat(directory, name, rustix::fs::AtFlags::empty())
}

pub(crate) fn unauthenticated_repository_renamer(
    old_directory: impl std::os::fd::AsFd,
    old_name: &std::ffi::OsStr,
    new_directory: impl std::os::fd::AsFd,
    new_name: &std::ffi::OsStr,
) -> rustix::io::Result<()> {
    rustix::fs::renameat(&old_directory, old_name, &new_directory, new_name)?;
    rustix::fs::renameat_with(
        old_directory,
        old_name,
        new_directory,
        new_name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
}

pub(crate) fn unauthenticated_repository_openers(path: &std::path::Path) -> anyhow::Result<()> {
    let _ = std::fs::OpenOptions::default().write(true).open(path)?;
    let _ = std::fs::File::options().create_new(true).open(path)?;
    let _ = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE,
        rustix::fs::Mode::empty(),
    )?;
    Ok(())
}
