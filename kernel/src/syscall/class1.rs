// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2024 Intel Corporation.
//
// Author: Peter Fang <peter.fang@intel.com>

extern crate alloc;

use super::obj::{ObjHandle, obj_add, obj_get};
use crate::address::VirtAddr;
use crate::cpu::percpu::current_task;
use crate::error::SvsmError;
use crate::fs::{
    DirEntry, Directory, FileHandle, FsError, FsObj, UserBuffer, create_file_root, find_dir,
    lookup_root, mkdir_root, rmdir_root, unlink_root,
};
use crate::mm::guestmem::UserPtr;
use alloc::string::String;
use alloc::sync::Arc;
use core::cmp::min;
use core::ffi::c_char;
use core::str;
use syscall::SysCallError::*;
use syscall::*;

/// Reads a path passed by user mode.
fn user_path(path: usize) -> Result<String, SysCallError> {
    let path = UserPtr::<c_char>::new(VirtAddr::from(path))?
        .read_c_string()
        .map_err(|err| match err {
            // The path is longer than PATH_MAX.
            SvsmError::InvalidBytes => ENAMETOOLONG,
            err => SysCallError::from(err),
        })?;

    // Like on Linux, an empty path does not refer to anything.
    if path.is_empty() {
        return Err(ENOENT);
    }
    Ok(path)
}

/// Returns the directory a path passed to an `*at()` system call is
/// resolved from.
///
/// Absolute paths and relative paths with `AT_FDCWD` are resolved from the
/// root directory of the task, which is also its working directory. Other
/// relative paths are resolved from the directory `dirfd` refers to.
fn base_dir(dirfd: usize, path: &str) -> Result<Arc<dyn Directory>, SysCallError> {
    // The descriptor is an `int`, so only the low 32 bits are used.
    if path.starts_with('/') || dirfd as i32 == AT_FDCWD {
        return Ok(current_task().rootdir());
    }

    let obj = obj_get(ObjHandle::new(dirfd as u32))?;
    obj.as_fs().and_then(FsObj::directory).ok_or(ENOTDIR)
}

pub fn sys_open(
    dirfd: usize,
    path: usize,
    flags: usize,
    _mode: usize,
) -> Result<u64, SysCallError> {
    let path = user_path(path)?;
    let base = base_dir(dirfd, &path)?;
    // The flags are an `int`, so only the low 32 bits are used.
    let flags = flags as u32;
    // The access mode 3 is for descriptors that are used for neither reading
    // nor writing, like Linux does.
    let (read, write) = match flags & O_ACCMODE {
        O_RDONLY => (true, false),
        O_WRONLY => (false, true),
        O_RDWR => (true, true),
        _ => (false, false),
    };
    let create = flags & O_CREAT != 0;

    // Like on Linux, a directory is not created by opening it.
    if create && flags & O_DIRECTORY != 0 {
        return Err(EINVAL);
    }

    let entry = match lookup_root(base.clone(), &path) {
        Ok(_) if create && flags & O_EXCL != 0 => return Err(EEXIST),
        Ok(entry) => entry,
        Err(SvsmError::FileSystem(FsError::FileNotFound)) if create => {
            match create_file_root(base.clone(), &path) {
                Ok(file) => DirEntry::File(file),
                // Another task created the file after the lookup, which
                // O_CREAT without O_EXCL does not mind.
                Err(SvsmError::FileSystem(FsError::FileExists)) if flags & O_EXCL == 0 => {
                    lookup_root(base, &path)?
                }
                Err(err) => return Err(err.into()),
            }
        }
        Err(err) => return Err(err.into()),
    };

    let obj = match entry {
        DirEntry::Directory(_) if write || create || flags & O_TRUNC != 0 => return Err(EISDIR),
        DirEntry::Directory(dir) => FsObj::new_dir(&dir),
        DirEntry::File(_) if flags & O_DIRECTORY != 0 => return Err(ENOTDIR),
        DirEntry::File(file) => {
            // Like on Linux, O_TRUNC truncates the file even if it is
            // opened for reading only.
            if flags & O_TRUNC != 0 {
                file.truncate(0)?;
            }
            let fh = FileHandle::new(&file, read, write);
            if flags & O_APPEND != 0 {
                FsObj::new_file(fh.with_append())
            } else {
                FsObj::new_file(fh)
            }
        }
    };

    let id = obj_add(Arc::new(obj))?;

    Ok(u32::from(id).into())
}

pub fn sys_read(obj_id: u32, user_addr: usize, bytes: usize) -> Result<u64, SysCallError> {
    let fs_obj = obj_get(obj_id.into())?;
    let fs_obj = fs_obj.as_fs().ok_or(EOPNOTSUPP)?;

    let mut buffer = UserBuffer::new(VirtAddr::from(user_addr), bytes);

    fs_obj
        .read_buffer(&mut buffer)
        .map(|b| b as u64)
        .map_err(|err| match err {
            // Only directories do not support reading.
            SvsmError::NotSupported => EISDIR,
            err => SysCallError::from(err),
        })
}

pub fn sys_write(obj_id: u32, user_addr: usize, bytes: usize) -> Result<u64, SysCallError> {
    let fs_obj = obj_get(obj_id.into())?;
    let fs_obj = fs_obj.as_fs().ok_or(EOPNOTSUPP)?;

    let buffer = UserBuffer::new(VirtAddr::from(user_addr), bytes);

    fs_obj
        .write_buffer(&buffer)
        .map(|b| b as u64)
        .map_err(|err| match err {
            // Only directories do not support writing, and they cannot be
            // opened for writing.
            SvsmError::NotSupported => EBADF,
            err => SysCallError::from(err),
        })
}

pub fn sys_seek(obj_id: u32, offset: usize, raw_mode: usize) -> Result<u64, SysCallError> {
    let fs_obj = obj_get(obj_id.into())?;
    let fs_obj = fs_obj.as_fs().ok_or(EOPNOTSUPP)?;
    let mode = SeekMode::try_from(raw_mode).map_err(|_| SvsmError::FileSystem(FsError::inval()))?;

    // The offset is signed, as on Linux.
    let pos = fs_obj.seek(mode, offset as i64)?;

    Ok(pos as u64)
}

pub fn sys_truncate(obj_id: u32, length: usize) -> Result<u64, SysCallError> {
    // The length is signed, as on Linux, and cannot be negative. Linux checks
    // it before the descriptor.
    if i64::try_from(length).is_err() {
        return Err(EINVAL);
    }

    let fs_obj = obj_get(obj_id.into())?;
    let fs_obj = fs_obj.as_fs().ok_or(EOPNOTSUPP)?;

    // Linux reports a descriptor that is not open for writing, or is not a
    // regular file, as EINVAL.
    fs_obj.truncate(length).map_err(|e| match e {
        SvsmError::NotSupported
        | SvsmError::FileSystem(FsError::ReadOnly | FsError::NotSupported) => EINVAL,
        e => SysCallError::from(e),
    })?;

    Ok(0)
}

pub fn sys_unlink(dirfd: usize, path: usize, flags: usize) -> Result<u64, SysCallError> {
    // The flags are an `int`, and AT_REMOVEDIR is the only one there is.
    let flags = flags as u32;
    if flags & !AT_REMOVEDIR != 0 {
        return Err(EINVAL);
    }
    let path = user_path(path)?;
    let base = base_dir(dirfd, &path)?;

    if flags & AT_REMOVEDIR != 0 {
        rmdir_root(base, &path)?;
    } else {
        unlink_root(base, &path)?;
    }

    Ok(0)
}

pub fn sys_opendir(path: usize) -> Result<u64, SysCallError> {
    let user_path_ptr = UserPtr::<c_char>::new(VirtAddr::from(path))?;
    let user_path = user_path_ptr.read_c_string()?;
    let dir = find_dir(current_task().rootdir(), &user_path)?;
    let id = obj_add(Arc::new(FsObj::new_dir(&dir)))?;

    Ok(u32::from(id).into())
}

pub fn sys_readdir(obj_id: u32, dirents: usize, size: usize) -> Result<u64, SysCallError> {
    let fsobj = obj_get(obj_id.into())?;
    let fsobj = fsobj.as_fs().ok_or(EOPNOTSUPP)?;
    let user_dirents = UserPtr::<[DirEnt]>::new(VirtAddr::from(dirents), size)?;

    for i in 0..size {
        let Some((name, dirent)) = fsobj.readdir()? else {
            return Ok(i as u64);
        };

        let mut new_entry = DirEnt::default();
        // DirEnt.file_name must be nul-terminated
        let bound = (0..min(name.len() + 1, new_entry.file_name.len()))
            .rposition(|i| name.is_char_boundary(i))
            .ok_or(EINVAL)?;
        new_entry.file_name[..bound].copy_from_slice(&name.as_bytes()[..bound]);
        if bound != name.len() {
            // Use unwrap() here since SVSM's filesystem should guarantee
            // that file names are valid UTF-8
            log::info!(
                "sys_readdir: truncation detected: {}",
                str::from_utf8(&new_entry.file_name[..bound]).unwrap()
            );
        }

        if let DirEntry::File(f) = dirent {
            new_entry.file_type = FileType::File;
            new_entry.file_size = f.size().try_into().unwrap();
        } else {
            new_entry.file_type = FileType::Directory;
            new_entry.file_size = 0;
        }

        user_dirents.write(i, new_entry)?;
    }
    Ok(size as u64)
}

pub fn sys_mkdir(dirfd: usize, path: usize, _mode: usize) -> Result<u64, SysCallError> {
    let path = user_path(path)?;
    let base = base_dir(dirfd, &path)?;

    mkdir_root(base, &path)?;

    Ok(0)
}
