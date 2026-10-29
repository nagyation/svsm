// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

//! Test for the execve() system call.
//!
//! A successful execve() does not return, so the test runs in two steps:
//! the first run checks the error cases and then executes the program
//! itself again, leaving a marker file. The second run finds the marker
//! file and checks the state of the task after execve().

#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU64, Ordering};
use userlib::*;

declare_main!(main);

/// Path of this program.
const PROGRAM: &CStr = c"/test/execve_tester";

/// Marker file telling the second run of the program from the first. It
/// holds the number of a file descriptor the first run left open.
const MARKER: &CStr = c"/execve_tester.marker";

/// Set by the first run before execve(). A new program starts with all
/// static data initialized again.
static STATE: AtomicU64 = AtomicU64::new(0);

/// Issues a system call with up to four arguments.
fn syscall(nr: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> Result<u64, SysCallError> {
    let ret: u64;
    // SAFETY: the callers only pass pointers the system call reads or
    // writes the way they intend. The registers clobbered by the
    // instruction are declared.
    unsafe {
        asm!(
            "syscall",
            inout("rax") nr => ret,
            in("rdi") arg1,
            in("rsi") arg2,
            in("r8") arg3,
            in("r9") arg4,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }

    // Errors are the values from -4095 to -1.
    let signed = ret as i64;
    if (-4095..=-1).contains(&signed) {
        Err(SysCallError::from(signed))
    } else {
        Ok(ret)
    }
}

fn open_path(path: &CStr, flags: u32) -> Result<u64, SysCallError> {
    let dirfd = AT_FDCWD as i64 as u64;
    syscall(SYS_OPEN, dirfd, path.as_ptr() as u64, flags.into(), 0o644)
}

fn write_fd(fd: u64, data: &[u8]) -> Result<u64, SysCallError> {
    syscall(SYS_WRITE, fd, data.as_ptr() as u64, data.len() as u64, 0)
}

fn read_fd(fd: u64, buf: &mut [u8]) -> Result<u64, SysCallError> {
    syscall(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64, 0)
}

/// Returns the current position of a file descriptor.
fn position(fd: u64) -> Result<u64, SysCallError> {
    syscall(SYS_SEEK, fd, 0, SeekMode::Relative as u64, 0)
}

fn close_fd(fd: u64) -> Result<u64, SysCallError> {
    syscall(SYS_CLOSE, fd, 0, 0, 0)
}

fn unlink_path(path: &CStr) -> Result<u64, SysCallError> {
    let dirfd = AT_FDCWD as i64 as u64;
    syscall(SYS_UNLINK, dirfd, path.as_ptr() as u64, 0, 0)
}

/// Issues execve() directly, to pass arguments `exec()` does not.
fn execve_raw(path: u64, argv: u64, envp: u64) -> Result<u64, SysCallError> {
    syscall(SYS_EXEC, path, argv, envp, 0)
}

/// Executes a program, and returns the error, as execve() only returns if it
/// fails.
fn execve(path: &CStr) -> SysCallError {
    match exec(path) {
        Err(err) => err,
        Ok(never) => match never {},
    }
}

/// Checks that execve() fails with `err` for a file with `content`.
fn check_execve_file(path: &CStr, content: &[u8], err: SysCallError) {
    let fd = open_path(path, O_WRONLY | O_CREAT | O_EXCL).unwrap();
    assert_eq!(write_fd(fd, content), Ok(content.len() as u64));
    assert_eq!(close_fd(fd), Ok(0));
    assert_eq!(execve(path), err);
    assert_eq!(unlink_path(path), Ok(0));
}

fn first_run() -> u32 {
    log::info!("Checking execve() errors");
    assert_eq!(execve(c"/execve_tester.none"), SysCallError::ENOENT);
    assert_eq!(
        execve_raw(0x0001_0000_0000_0000, 0, 0),
        Err(SysCallError::EFAULT)
    );
    // The arguments and the environment are not supported yet.
    let path = PROGRAM.as_ptr() as u64;
    assert_eq!(execve_raw(path, 1, 0), Err(SysCallError::EINVAL));
    assert_eq!(execve_raw(path, 0, 1), Err(SysCallError::EINVAL));
    // Only regular files can be executed.
    assert_eq!(execve(c"/test"), SysCallError::EACCES);
    check_execve_file(c"/execve_tester.empty", b"", SysCallError::ENOEXEC);
    check_execve_file(
        c"/execve_tester.txt",
        b"not an ELF file",
        SysCallError::ENOEXEC,
    );

    // Leave a file descriptor open, which survives execve(), and record
    // its number in the marker file.
    let fd = open_path(MARKER, O_RDWR | O_CREAT | O_EXCL).unwrap();
    assert_eq!(write_fd(fd, &fd.to_le_bytes()), Ok(8));

    log::info!("Executing {PROGRAM:?}");
    STATE.store(1, Ordering::Relaxed);
    let err = execve(PROGRAM);
    panic!("execve() failed with {err:?}");
}

fn second_run(marker_fd: u64) -> u32 {
    log::info!("Checking the state after execve()");
    assert_eq!(STATE.load(Ordering::Relaxed), 0);

    // The file descriptor of the first run is still open, at the same
    // position.
    let mut buf = [0u8; 8];
    assert_eq!(read_fd(marker_fd, &mut buf), Ok(8));
    let inherited_fd = u64::from_le_bytes(buf);
    assert_eq!(position(inherited_fd), Ok(8));

    assert_eq!(close_fd(inherited_fd), Ok(0));
    assert_eq!(close_fd(marker_fd), Ok(0));
    assert_eq!(unlink_path(MARKER), Ok(0));

    log::info!("execve() tests passed");

    // The kernel log does not depend on the standard descriptors, which the
    // program can close: the exit message still shows up.
    assert_eq!(close_fd(STDOUT_FILENO as u64), Ok(0));
    assert_eq!(close_fd(STDERR_FILENO as u64), Ok(0));
    0
}

fn main() -> u32 {
    match open_path(MARKER, O_RDONLY) {
        Err(SysCallError::ENOENT) => first_run(),
        Ok(fd) => second_run(fd),
        Err(err) => panic!("Failed to open {MARKER:?}: {err:?}"),
    }
}
