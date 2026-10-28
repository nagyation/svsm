// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

//! Tests for the file system calls.

use userlib::{FileFlags, FileModes, SeekMode, SysCallError, open, read, seek, unlink, write};

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
