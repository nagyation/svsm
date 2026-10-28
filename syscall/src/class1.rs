// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2024 Intel Corporation.
//
// Author: Peter Fang <peter.fang@intel.com>

use super::call::{SysCallError, syscall2, syscall3, syscall4};
use super::def::{
    AT_FDCWD, AT_REMOVEDIR, DT_DIR, Dirent64Header, F_NAME_SIZE, FileFlags, FileModes, FileType,
    O_APPEND, O_CREAT, O_DIRECTORY, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, SYS_MKDIR, SYS_OPEN,
    SYS_READ, SYS_READDIR, SYS_SEEK, SYS_TRUNCATE, SYS_UNLINK, SYS_WRITE, SeekMode,
};
use super::{DirEnt, Obj, ObjHandle};
use core::ffi::CStr;
use core::mem::size_of;
use zerocopy::FromBytes;

#[derive(Debug)]
pub struct FsObjHandle(ObjHandle);

impl FsObjHandle {
    pub(crate) const fn new(obj: ObjHandle) -> Self {
        Self(obj)
    }
}

impl Obj for FsObjHandle {
    fn id(&self) -> u32 {
        u32::from(&self.0)
    }
}

/// Opens a directory with the Linux `openat()` system call and `O_DIRECTORY`.
pub fn opendir(path: &CStr) -> Result<FsObjHandle, SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe {
        syscall4(
            SYS_OPEN,
            AT_FDCWD as i64 as u64,
            path.as_ptr() as u64,
            (O_RDONLY | O_DIRECTORY).into(),
            0,
        )
        .map(|ret| FsObjHandle(ObjHandle::new(ret as u32)))
    }
}

/// Converts a directory entry returned by the readdir system call into a
/// [`DirEnt`].
fn dirent(name: &[u8], d_type: u8) -> DirEnt {
    let mut entry = DirEnt::default();
    // The name stays NUL-terminated.
    let len = name.len().min(F_NAME_SIZE - 1);
    entry.file_name[..len].copy_from_slice(&name[..len]);
    entry.file_type = if d_type == DT_DIR {
        FileType::Directory
    } else {
        FileType::File
    };
    entry
}

/// Reads the next entries of a directory, with the Linux `getdents64()`
/// system call. Returns the number of entries stored in `dirents`, which is 0
/// at the end of the directory.
pub fn readdir(fs: &FsObjHandle, dirents: &mut [DirEnt]) -> Result<usize, SysCallError> {
    // Large enough for at least one entry with a name of maximum length.
    let mut buf = [0u8; 512];
    let mut count = 0;

    while count < dirents.len() {
        // SAFETY: The system call writes at most `buf.len()` bytes to `buf`.
        let len = unsafe {
            syscall3(
                SYS_READDIR,
                fs.id().into(),
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        }? as usize;
        if len == 0 {
            break;
        }

        let mut pos = 0;
        let mut last_off = 0;
        while pos < len {
            if count == dirents.len() {
                // Continue after the last entry stored with the next call, so
                // that the remaining entries are not lost.
                seek(fs, last_off, SeekMode::Absolute)?;
                return Ok(count);
            }

            let (header, _) =
                Dirent64Header::read_from_prefix(&buf[pos..len]).map_err(|_| SysCallError::EIO)?;
            let reclen = usize::from(header.d_reclen);
            let name_start = pos + size_of::<Dirent64Header>();
            if reclen <= size_of::<Dirent64Header>() || pos + reclen > len {
                return Err(SysCallError::EIO);
            }
            let name = &buf[name_start..pos + reclen];
            let name_len = name.iter().position(|&c| c == 0).unwrap_or(name.len());

            dirents[count] = dirent(&name[..name_len], header.d_type);
            count += 1;
            last_off = header.d_off;
            pos += reclen;
        }
    }

    Ok(count)
}

/// Opens a file with the Linux `openat()` system call. A mode without
/// `FileModes::READ` or `FileModes::WRITE` opens the file for reading.
pub fn open(path: &CStr, mode: FileModes, flags: FileFlags) -> Result<FsObjHandle, SysCallError> {
    let mut open_flags = match (
        mode.contains(FileModes::READ),
        mode.contains(FileModes::WRITE),
    ) {
        (true, true) => O_RDWR,
        (false, true) => O_WRONLY,
        (_, false) => O_RDONLY,
    };
    if mode.contains(FileModes::TRUNC) {
        open_flags |= O_TRUNC;
    }
    if mode.contains(FileModes::APPEND) {
        open_flags |= O_APPEND;
    }
    if flags.contains(FileFlags::CREATE) {
        open_flags |= O_CREAT;
    }

    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe {
        syscall4(
            SYS_OPEN,
            AT_FDCWD as i64 as u64,
            path.as_ptr() as u64,
            open_flags.into(),
            0,
        )
        .map(|ret| FsObjHandle(ObjHandle::new(ret as u32)))
    }
}

pub fn read(fd: &FsObjHandle, buffer: &mut [u8]) -> Result<usize, SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process. All memory changes happen from kernel context.
    unsafe {
        syscall3(
            SYS_READ,
            fd.id().into(),
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
        .map(|ret| ret.try_into().unwrap())
    }
}

pub fn write(fd: &FsObjHandle, buffer: &[u8]) -> Result<usize, SysCallError> {
    // SAFETY: Invokes a system call and does not change memory of the process.
    // Kernel will only read from process memory.
    unsafe {
        syscall3(
            SYS_WRITE,
            fd.id().into(),
            buffer.as_ptr() as u64,
            buffer.len() as u64,
        )
        .map(|ret| ret.try_into().unwrap())
    }
}

/// Changes the file offset like lseek() and returns the new one. The offset
/// is signed and added to the start of the file, the current offset or the
/// end of the file, depending on the mode, so a negative offset with
/// `SeekMode::End` goes back from the end. The new offset can be after the end
/// of the file, but not negative.
pub fn seek(fd: &FsObjHandle, offset: i64, mode: SeekMode) -> Result<u64, SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe { syscall3(SYS_SEEK, fd.id().into(), offset as u64, mode as u64) }
}

/// Changes the size of the file like ftruncate(). A larger size extends the
/// file with zeros. The file offset does not change.
pub fn truncate(fd: &FsObjHandle, length: u64) -> Result<u64, SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe { syscall2(SYS_TRUNCATE, fd.id().into(), length) }
}

/// Removes a file with the Linux `unlinkat()` system call.
pub fn unlink(path: &CStr) -> Result<(), SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe { syscall3(SYS_UNLINK, AT_FDCWD as i64 as u64, path.as_ptr() as u64, 0).map(|_| ()) }
}

/// Creates a directory with the Linux `mkdirat()` system call.
pub fn mkdir(path: &CStr) -> Result<(), SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe { syscall3(SYS_MKDIR, AT_FDCWD as i64 as u64, path.as_ptr() as u64, 0).map(|_| ()) }
}

/// Removes a directory with the Linux `unlinkat()` system call.
pub fn rmdir(path: &CStr) -> Result<(), SysCallError> {
    // SAFETY: Invokes a system call and does not directly change any memory of
    // the process.
    unsafe {
        syscall3(
            SYS_UNLINK,
            AT_FDCWD as i64 as u64,
            path.as_ptr() as u64,
            AT_REMOVEDIR.into(),
        )
        .map(|_| ())
    }
}
