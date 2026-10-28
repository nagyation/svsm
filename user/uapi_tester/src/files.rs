// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

//! Tests for the file system calls.

use super::syscall::syscall_insn3;
use userlib::{
    FileFlags, FileModes, SYS_TRUNCATE, SeekMode, SysCallError, open, opendir, read, seek,
    truncate, unlink, write,
};

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
