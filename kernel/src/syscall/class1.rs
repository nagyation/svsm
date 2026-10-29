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
    DirEntry, Directory, FileHandle, FsError, FsObj, UserBuffer, create_file_root, lookup_root,
    mkdir_root, rmdir_root, unlink_root,
};
use crate::mm::guestmem::{UserPtr, copy_to_user};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use core::ffi::c_char;
use syscall::SysCallError::*;
use syscall::*;
use zerocopy::IntoBytes;

/// Reads a path passed by user mode.
pub(super) fn user_path(path: usize) -> Result<String, SysCallError> {
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

pub fn sys_readdir(obj_id: u32, dirp: usize, count: usize) -> Result<u64, SysCallError> {
    let obj = obj_get(obj_id.into())?;
    let fs_obj = obj
        .as_fs()
        .filter(|fs_obj| fs_obj.directory().is_some())
        .ok_or(ENOTDIR)?;
    let buf = VirtAddr::from(dirp);
    // The count is an `unsigned int`, so only the low 32 bits are used.
    let count = count as u32 as usize;

    let mut pos = fs_obj.dir_position()?;
    let mut written = 0;
    loop {
        let (name, entry) = match fs_obj.dir_entry_at(pos) {
            Ok(Some(dirent)) => dirent,
            Ok(None) => break,
            // The entry was removed since the directory was opened.
            Err(SvsmError::FileSystem(FsError::FileNotFound)) => {
                pos += 1;
                continue;
            }
            Err(err) => return Err(err.into()),
        };

        let reclen = Dirent64Header::reclen(name.len());
        let d_reclen = u16::try_from(reclen).map_err(|_| ENAMETOOLONG)?;
        if reclen > count - written {
            // Like Linux, fail if not even a single entry fits.
            if written == 0 {
                return Err(EINVAL);
            }
            break;
        }

        // The SVSM file system has no inode numbers, so report the position
        // of the entry, which is never 0 like a deleted entry. The offset is
        // the position of the next entry, as lseek() uses it for
        // directories.
        let header = Dirent64Header {
            d_ino: pos as u64 + 1,
            d_off: pos as i64 + 1,
            d_reclen,
            d_type: if entry.is_dir() { DT_DIR } else { DT_REG },
        };
        let name_start = header.as_bytes().len();
        let mut record = vec![0u8; reclen];
        record[..name_start].copy_from_slice(header.as_bytes());
        record[name_start..name_start + name.len()].copy_from_slice(name.as_bytes());
        match copy_to_user(&record, buf + written) {
            Ok(()) => {}
            // Like Linux, report the entries that were written before.
            Err(_) if written > 0 => break,
            Err(err) => return Err(err.into()),
        }

        written += reclen;
        pos += 1;
    }

    fs_obj.dir_seek(pos)?;

    Ok(written as u64)
}

pub fn sys_mkdir(dirfd: usize, path: usize, _mode: usize) -> Result<u64, SysCallError> {
    let path = user_path(path)?;
    let base = base_dir(dirfd, &path)?;

    mkdir_root(base, &path)?;

    Ok(0)
}
