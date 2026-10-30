// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

//! Tests for the file descriptors of a task.

use super::syscall::{syscall_insn, syscall_insn3};
use userlib::{FileFlags, FileModes, Obj, SysCallError, SysNo, open, unlink};

/// Checks that standard output and standard error, the file descriptors 1 and
/// 2, are the log and accept writes, and that standard input, the file
/// descriptor 0, is open, always at the end and not writable.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_log_descriptors() {
    let msg = b"test_log_descriptors\n";

    for fd in [1, 2] {
        let ret = syscall_insn3(
            SysNo::Write as u64,
            fd,
            msg.as_ptr() as u64,
            msg.len() as u64,
        );
        assert_eq!(ret, msg.len() as u64);
    }

    let ret = syscall_insn3(
        SysNo::Write as u64,
        0,
        msg.as_ptr() as u64,
        msg.len() as u64,
    );
    assert_eq!(ret as i64, SysCallError::EBADF as i64);

    let mut buf = [0u8; 8];
    let ret = syscall_insn3(
        SysNo::Read as u64,
        0,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    assert_eq!(ret, 0);
}

/// Checks that closing a file descriptor that is not open fails with EBADF,
/// like on Linux, also when it was already closed.
#[test]
#[cfg_attr(not(test_in_svsm), ignore = "Can only be run inside guest")]
fn test_close_bad_descriptor() {
    let bad = syscall_insn(SysNo::Close as u64, 1000);
    assert_eq!(bad as i64, SysCallError::EBADF as i64);

    let path = c"/test_close_bad_descriptor";
    let file = open(path, FileModes::WRITE, FileFlags::CREATE).unwrap();
    let fd = u64::from(file.id());
    // Dropping the handle closes the file descriptor.
    drop(file);
    let again = syscall_insn(SysNo::Close as u64, fd);
    assert_eq!(again as i64, SysCallError::EBADF as i64);

    unlink(path).unwrap();
}
