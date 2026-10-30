// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2024 Intel Corporation.
//
// Author: Chuanxiao Dong <chuanxiao.dong@intel.com>

use super::SysNo;
use super::call::{SysCallError, syscall1, syscall3};
use core::convert::Infallible;
use core::ffi::CStr;

/// Terminate the current task with the given exit code. Like on Linux, only
/// the low 8 bits of the code are used.
pub fn exit(code: u32) -> ! {
    // SAFETY: exit_group is a system call of the svsm kernel.
    unsafe {
        let _ = syscall1(SysNo::ExitGroup as u64, u64::from(code));
    }
    unreachable!("Should never return from the exit_group system call");
}

/// Replaces the program of the calling task, like the Linux `execve()` system
/// call. The task keeps its open file descriptors. It does not return if the
/// program is executed, and returns the error otherwise. The arguments and the
/// environment of the new program are not supported yet.
pub fn exec(file: &CStr) -> Result<Infallible, SysCallError> {
    // SAFETY: Invokes a system call that only reads the path, which is a
    // NUL-terminated string. If it succeeds, the program is replaced and the
    // call does not return.
    match unsafe { syscall3(SysNo::Execve as u64, file.as_ptr() as u64, 0, 0) } {
        Err(err) => Err(err),
        Ok(_) => unreachable!("execve() does not return if it succeeds"),
    }
}
