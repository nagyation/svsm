// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

//! Tests for the file system calls.

use super::syscall::{syscall_insn, syscall_insn3, syscall_insn4};
use core::ffi::CStr;
use userlib::{
    AT_FDCWD, AT_REMOVEDIR, FileFlags, FileModes, O_ACCMODE, O_APPEND, O_CREAT, O_DIRECTORY,
    O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, SYS_CLOSE, SYS_MKDIR, SYS_OPEN, SYS_READ,
    SYS_SEEK, SYS_TRUNCATE, SYS_UNLINK, SYS_WRITE, SeekMode, SysCallError, mkdir, open, opendir,
    read, rmdir, seek, truncate, unlink, write,
};

/// Opens a path with the open system call, which works like `openat()`, and
/// returns the file descriptor, or the negative error number.
fn raw_open(dirfd: i32, path: &CStr, flags: u32) -> i64 {
    let dirfd = i64::from(dirfd) as u64;
    syscall_insn4(SYS_OPEN, dirfd, path.as_ptr() as u64, flags.into(), 0) as i64
}

/// Closes a file descriptor.
fn raw_close(fd: i64) {
    assert_eq!(syscall_insn(SYS_CLOSE, fd as u64), 0);
}

/// Reads from a file descriptor, and returns the number of bytes or the
/// negative error number.
fn raw_read(fd: i64, buf: &mut [u8]) -> i64 {
    syscall_insn3(
        SYS_READ,
        fd as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ) as i64
}

/// Writes to a file descriptor, and returns the number of bytes or the
/// negative error number.
fn raw_write(fd: i64, buf: &[u8]) -> i64 {
    syscall_insn3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as i64
}

/// Returns the negative value of an error, as the system calls return it.
fn err(error: SysCallError) -> i64 {
    error as i64
}

/// Checks that seeking follows Linux: the offset is signed and added to the
/// start, the current position or the end of the file, a negative result is
/// an error, and the position can be after the end of the file.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_seek() {
    let path = c"/test_seek";
    let modes = FileModes::READ | FileModes::WRITE;
    let file = open(path, modes, FileFlags::CREATE).unwrap();
    assert_eq!(write(&file, b"0123456789"), Ok(10));

    assert_eq!(seek(&file, 0, SeekMode::Absolute), Ok(0));
    assert_eq!(seek(&file, 3, SeekMode::Relative), Ok(3));
    assert_eq!(seek(&file, -1, SeekMode::Relative), Ok(2));
    assert_eq!(seek(&file, 0, SeekMode::End), Ok(10));
    assert_eq!(seek(&file, -2, SeekMode::End), Ok(8));
    let mut buf = [0u8; 16];
    assert_eq!(read(&file, &mut buf), Ok(2));
    assert_eq!(&buf[..2], b"89");

    // A negative result is an error and the position does not change.
    assert_eq!(
        seek(&file, -1, SeekMode::Absolute),
        Err(SysCallError::EINVAL)
    );
    assert_eq!(seek(&file, -11, SeekMode::End), Err(SysCallError::EINVAL));
    assert_eq!(seek(&file, 0, SeekMode::Relative), Ok(10));

    // The position can be after the end of the file. Reading there returns
    // no data, and writing extends the file with zeros.
    assert_eq!(seek(&file, 20, SeekMode::Absolute), Ok(20));
    assert_eq!(read(&file, &mut buf), Ok(0));
    assert_eq!(write(&file, b"x"), Ok(1));
    assert_eq!(seek(&file, 0, SeekMode::End), Ok(21));
    assert_eq!(seek(&file, 8, SeekMode::Absolute), Ok(8));
    assert_eq!(read(&file, &mut buf), Ok(13));
    assert_eq!(&buf[..13], b"89\0\0\0\0\0\0\0\0\0\0x");

    // The size of a file is limited, so a write far after its end fails.
    assert_eq!(seek(&file, 1 << 40, SeekMode::Absolute), Ok(1 << 40));
    assert_eq!(write(&file, b"x"), Err(SysCallError::EFBIG));

    drop(file);
    unlink(path).unwrap();
}

/// Checks that truncating follows Linux: the file shrinks or is extended with
/// zeros, the file offset does not change, and the call returns 0 or fails with
/// EINVAL for a length that is too large, a file that is not open for writing
/// and a directory.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_truncate() {
    let path = c"/test_truncate";
    let modes = FileModes::READ | FileModes::WRITE;
    let file = open(path, modes, FileFlags::CREATE).unwrap();
    assert_eq!(write(&file, b"0123456789"), Ok(10));

    assert_eq!(truncate(&file, 4), Ok(0));
    assert_eq!(seek(&file, 0, SeekMode::End), Ok(4));
    // The file offset stays where the write left it.
    assert_eq!(seek(&file, 10, SeekMode::Absolute), Ok(10));
    assert_eq!(truncate(&file, 8), Ok(0));
    assert_eq!(seek(&file, 0, SeekMode::Relative), Ok(10));

    // The file was extended with zeros.
    assert_eq!(seek(&file, 0, SeekMode::Absolute), Ok(0));
    let mut buf = [0xffu8; 16];
    assert_eq!(read(&file, &mut buf), Ok(8));
    assert_eq!(&buf[..8], b"0123\0\0\0\0");

    // A negative length, as a signed value, is invalid, also for a descriptor
    // that is not open. The size of a file is limited.
    assert_eq!(truncate(&file, u64::MAX), Err(SysCallError::EINVAL));
    let ret = syscall_insn3(SYS_TRUNCATE, 1000, u64::MAX, 0);
    assert_eq!(ret as i64, SysCallError::EINVAL as i64);
    let ret = syscall_insn3(SYS_TRUNCATE, 1000, 0, 0);
    assert_eq!(ret as i64, SysCallError::EBADF as i64);
    assert_eq!(truncate(&file, 1 << 40), Err(SysCallError::EFBIG));
    drop(file);

    let file = open(path, FileModes::READ, FileFlags::empty()).unwrap();
    assert_eq!(truncate(&file, 0), Err(SysCallError::EINVAL));
    drop(file);

    let dir = opendir(c"/").unwrap();
    assert_eq!(truncate(&dir, 0), Err(SysCallError::EINVAL));
    drop(dir);

    unlink(path).unwrap();
}

/// Checks that the access mode of the open system call is enforced like on
/// Linux: reads and writes that the mode does not allow fail with EBADF, also
/// for a file that the call creates.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_open_access_modes() {
    let path = c"/test_open_access_modes";
    let mut buf = [0u8; 8];

    let fd = raw_open(AT_FDCWD, path, O_WRONLY | O_CREAT);
    assert!(fd >= 0);
    assert_eq!(raw_write(fd, b"abc"), 3);
    assert_eq!(raw_read(fd, &mut buf), err(SysCallError::EBADF));
    raw_close(fd);

    let fd = raw_open(AT_FDCWD, path, O_RDONLY);
    assert!(fd >= 0);
    assert_eq!(raw_read(fd, &mut buf), 3);
    assert_eq!(raw_write(fd, b"x"), err(SysCallError::EBADF));
    raw_close(fd);

    let fd = raw_open(AT_FDCWD, path, O_RDWR);
    assert!(fd >= 0);
    assert_eq!(raw_write(fd, b"def"), 3);
    assert_eq!(raw_read(fd, &mut buf), 0);
    raw_close(fd);

    // A file created for reading is not open for writing.
    let other = c"/test_open_access_modes_ro";
    let fd = raw_open(AT_FDCWD, other, O_RDONLY | O_CREAT);
    assert!(fd >= 0);
    assert_eq!(raw_write(fd, b"x"), err(SysCallError::EBADF));
    raw_close(fd);

    // The access mode 3, like on Linux, is for a descriptor that is used for
    // neither reading nor writing.
    let fd = raw_open(AT_FDCWD, path, O_ACCMODE);
    assert!(fd >= 0);
    assert_eq!(raw_read(fd, &mut buf), err(SysCallError::EBADF));
    assert_eq!(raw_write(fd, b"x"), err(SysCallError::EBADF));
    raw_close(fd);

    unlink(path).unwrap();
    unlink(other).unwrap();
}

/// Checks the errors of creating files with the open system call: a file that
/// does not exist is only created with O_CREAT, O_EXCL fails if it exists,
/// and the path must have a parent directory and no file as a directory.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_open_create_flags() {
    let path = c"/test_open_create_flags";

    assert_eq!(
        raw_open(AT_FDCWD, path, O_RDONLY),
        err(SysCallError::ENOENT)
    );

    let fd = raw_open(AT_FDCWD, path, O_RDWR | O_CREAT | O_EXCL);
    assert!(fd >= 0);
    raw_close(fd);
    assert_eq!(
        raw_open(AT_FDCWD, path, O_RDWR | O_CREAT | O_EXCL),
        err(SysCallError::EEXIST)
    );
    // Without O_EXCL, O_CREAT opens an existing file.
    let fd = raw_open(AT_FDCWD, path, O_RDWR | O_CREAT);
    assert!(fd >= 0);
    raw_close(fd);

    assert_eq!(
        raw_open(AT_FDCWD, c"/no_such_dir/file", O_RDWR | O_CREAT),
        err(SysCallError::ENOENT)
    );
    assert_eq!(
        raw_open(AT_FDCWD, c"/test_open_create_flags/file", O_RDONLY),
        err(SysCallError::ENOTDIR)
    );
    assert_eq!(raw_open(AT_FDCWD, c"", O_RDONLY), err(SysCallError::ENOENT));

    // A directory is not created with O_CREAT, and nothing is left behind.
    let dir = c"/test_open_create_flags_dir";
    assert_eq!(
        raw_open(AT_FDCWD, dir, O_RDONLY | O_CREAT | O_DIRECTORY),
        err(SysCallError::EINVAL)
    );
    assert_eq!(raw_open(AT_FDCWD, dir, O_RDONLY), err(SysCallError::ENOENT));

    unlink(path).unwrap();
}

/// Checks the limit of the length of a name: a name of 255 bytes works, a
/// longer one fails with ENAMETOOLONG, also with O_CREAT.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_open_name_length() {
    // The path is a slash, the name and a NUL.
    let mut path = [b'a'; 258];
    path[0] = b'/';

    path[257] = 0;
    let too_long = CStr::from_bytes_with_nul(&path[..258]).unwrap();
    assert_eq!(
        raw_open(AT_FDCWD, too_long, O_RDONLY),
        err(SysCallError::ENAMETOOLONG)
    );
    assert_eq!(
        raw_open(AT_FDCWD, too_long, O_RDWR | O_CREAT),
        err(SysCallError::ENAMETOOLONG)
    );

    path[256] = 0;
    let longest = CStr::from_bytes_with_nul(&path[..257]).unwrap();
    let fd = raw_open(AT_FDCWD, longest, O_RDWR | O_CREAT);
    assert!(fd >= 0);
    raw_close(fd);
    unlink(longest).unwrap();
}

/// Checks O_TRUNC and O_APPEND: O_TRUNC empties the file, and with O_APPEND
/// every write goes to the end of the file, also after seeking to the start.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_open_trunc_append() {
    let path = c"/test_open_trunc_append";
    let mut buf = [0u8; 16];

    let fd = raw_open(AT_FDCWD, path, O_WRONLY | O_CREAT);
    assert_eq!(raw_write(fd, b"abcdef"), 6);
    raw_close(fd);

    let fd = raw_open(AT_FDCWD, path, O_WRONLY | O_APPEND);
    assert_eq!(raw_write(fd, b"12"), 2);
    assert_eq!(syscall_insn3(SYS_SEEK, fd as u64, 0, 0), 0);
    assert_eq!(raw_write(fd, b"34"), 2);
    raw_close(fd);

    let fd = raw_open(AT_FDCWD, path, O_RDONLY);
    assert_eq!(raw_read(fd, &mut buf), 10);
    assert_eq!(&buf[..10], b"abcdef1234");
    raw_close(fd);

    let fd = raw_open(AT_FDCWD, path, O_WRONLY | O_TRUNC);
    assert!(fd >= 0);
    raw_close(fd);
    let fd = raw_open(AT_FDCWD, path, O_RDONLY);
    assert_eq!(raw_read(fd, &mut buf), 0);
    raw_close(fd);

    // Like on Linux, O_TRUNC also empties a file opened for reading.
    let fd = raw_open(AT_FDCWD, path, O_RDWR);
    assert_eq!(raw_write(fd, b"xyz"), 3);
    raw_close(fd);
    let fd = raw_open(AT_FDCWD, path, O_RDONLY | O_TRUNC);
    assert_eq!(raw_read(fd, &mut buf), 0);
    raw_close(fd);

    unlink(path).unwrap();
}

/// Checks opening directories: a directory can be opened for reading, but
/// not read, and not for writing; O_DIRECTORY needs a directory; and a
/// relative path is resolved from the directory descriptor, unless it is an
/// absolute path.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_open_directory() {
    let dir = c"/test_open_directory";
    let file = c"/test_open_directory/file";
    let mut buf = [0u8; 8];

    mkdir(dir).unwrap();
    let fd = raw_open(AT_FDCWD, file, O_RDWR | O_CREAT);
    assert!(fd >= 0);
    raw_close(fd);

    let dirfd = raw_open(AT_FDCWD, dir, O_RDONLY);
    assert!(dirfd >= 0);
    assert_eq!(raw_read(dirfd, &mut buf), err(SysCallError::EISDIR));
    assert_eq!(raw_write(dirfd, b"x"), err(SysCallError::EBADF));
    raw_close(dirfd);

    assert_eq!(raw_open(AT_FDCWD, dir, O_WRONLY), err(SysCallError::EISDIR));
    assert_eq!(raw_open(AT_FDCWD, dir, O_RDWR), err(SysCallError::EISDIR));
    assert_eq!(
        raw_open(AT_FDCWD, dir, O_RDONLY | O_TRUNC),
        err(SysCallError::EISDIR)
    );
    assert_eq!(
        raw_open(AT_FDCWD, dir, O_RDONLY | O_CREAT),
        err(SysCallError::EISDIR)
    );
    assert_eq!(
        raw_open(AT_FDCWD, file, O_RDONLY | O_DIRECTORY),
        err(SysCallError::ENOTDIR)
    );

    let dirfd = raw_open(AT_FDCWD, dir, O_RDONLY | O_DIRECTORY);
    assert!(dirfd >= 0);
    let fd = raw_open(dirfd as i32, c"file", O_RDONLY);
    assert!(fd >= 0);
    raw_close(fd);
    // An absolute path does not use the directory descriptor.
    let fd = raw_open(dirfd as i32, file, O_RDONLY);
    assert!(fd >= 0);
    raw_close(fd);
    raw_close(dirfd);

    // A descriptor that is not open, or not a directory.
    assert_eq!(raw_open(1000, c"file", O_RDONLY), err(SysCallError::EBADF));
    let filefd = raw_open(AT_FDCWD, file, O_RDONLY);
    assert_eq!(
        raw_open(filefd as i32, c"other", O_RDONLY),
        err(SysCallError::ENOTDIR)
    );
    raw_close(filefd);

    unlink(file).unwrap();
    rmdir(dir).unwrap();
}

/// Checks the errors of creating and removing directories and files, which
/// follow Linux: EEXIST, ENOENT, ENOTDIR, EISDIR and ENOTEMPTY.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_mkdir_unlink_errors() {
    let dir = c"/test_mkdir_unlink_errors";
    let file = c"/test_mkdir_unlink_errors/file";

    mkdir(dir).unwrap();
    assert_eq!(mkdir(dir), Err(SysCallError::EEXIST));
    assert_eq!(mkdir(c"/no_such_dir/sub"), Err(SysCallError::ENOENT));

    let fd = raw_open(AT_FDCWD, file, O_RDWR | O_CREAT);
    assert!(fd >= 0);
    raw_close(fd);
    assert_eq!(
        mkdir(c"/test_mkdir_unlink_errors/file/sub"),
        Err(SysCallError::ENOTDIR)
    );

    // A directory is removed with AT_REMOVEDIR, a file without it.
    assert_eq!(unlink(dir), Err(SysCallError::EISDIR));
    assert_eq!(rmdir(file), Err(SysCallError::ENOTDIR));
    assert_eq!(rmdir(dir), Err(SysCallError::ENOTEMPTY));

    unlink(file).unwrap();
    assert_eq!(unlink(file), Err(SysCallError::ENOENT));
    rmdir(dir).unwrap();
    assert_eq!(rmdir(dir), Err(SysCallError::ENOENT));
}

/// Checks that mkdirat() and unlinkat() resolve a relative path from the
/// directory descriptor, and that unlinkat() accepts only AT_REMOVEDIR.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_mkdir_unlink_at() {
    let dir = c"/test_mkdir_unlink_at";
    mkdir(dir).unwrap();
    let dirfd = raw_open(AT_FDCWD, dir, O_RDONLY | O_DIRECTORY);
    assert!(dirfd >= 0);
    let dirfd_arg = dirfd as u64;

    let ret = syscall_insn3(SYS_MKDIR, dirfd_arg, c"sub".as_ptr() as u64, 0);
    assert_eq!(ret, 0);
    // The directory was created in the directory the descriptor refers to.
    let fd = raw_open(
        AT_FDCWD,
        c"/test_mkdir_unlink_at/sub",
        O_RDONLY | O_DIRECTORY,
    );
    assert!(fd >= 0);
    raw_close(fd);

    // Only AT_REMOVEDIR is a valid flag.
    let ret = syscall_insn3(SYS_UNLINK, dirfd_arg, c"sub".as_ptr() as u64, 1);
    assert_eq!(ret as i64, err(SysCallError::EINVAL));
    let ret = syscall_insn3(SYS_UNLINK, dirfd_arg, c"sub".as_ptr() as u64, 0);
    assert_eq!(ret as i64, err(SysCallError::EISDIR));
    let remove = u64::from(AT_REMOVEDIR);
    let ret = syscall_insn3(SYS_UNLINK, dirfd_arg, c"sub".as_ptr() as u64, remove);
    assert_eq!(ret, 0);

    // A descriptor that is not open.
    let ret = syscall_insn3(SYS_MKDIR, 1000, c"sub".as_ptr() as u64, 0);
    assert_eq!(ret as i64, err(SysCallError::EBADF));

    raw_close(dirfd);
    rmdir(dir).unwrap();
}
