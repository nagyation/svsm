// SPDX-License-Identifier: MIT
//
// Copyright (c) 2024 SUSE LLC
//
// Author: Joerg Roedel <jroedel@suse.de>

use super::class1::user_path;
use super::obj::obj_close;
use crate::cpu::X86ExceptionContext;
use crate::cpu::percpu::current_task;
use crate::error::SvsmError;
use crate::fs::FsError;
use crate::task::TaskExitStatus;
use crate::task::exec_current;
use crate::task::terminate;
use syscall::SysCallError;

/// Returns the exit status of a task that exits with `exit_code`. Like on
/// Linux, only the low 8 bits of the exit code are kept.
fn exit_status(exit_code: u32) -> u8 {
    (exit_code & 0xff) as u8
}

pub fn sys_exit(exit_code: u32) -> ! {
    let code = u16::from(exit_status(exit_code));
    log::info!(
        "Terminating task {}, exit_code {code}",
        current_task().get_task_name()
    );
    terminate(Some(TaskExitStatus::Exited(code)));
}

pub fn sys_exec(
    ctxt: &mut X86ExceptionContext,
    file: usize,
    argv: usize,
    envp: usize,
) -> Result<u64, SysCallError> {
    let path = user_path(file)?;

    // The arguments and the environment are not passed to the program yet.
    if argv != 0 || envp != 0 {
        return Err(SysCallError::EINVAL);
    }

    // On success the context enters the new program, whose registers are
    // cleared, so the result is 0.
    exec_current(ctxt, &path).map_err(|err| match err {
        // Linux refuses to execute anything but a regular file with EACCES.
        SvsmError::FileSystem(FsError::IsDir) => SysCallError::EACCES,
        err => err.into(),
    })?;

    Ok(0)
}

pub fn sys_close(obj_id: u32) -> Result<u64, SysCallError> {
    obj_close(obj_id.into())?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_status() {
        assert_eq!(exit_status(0), 0);
        assert_eq!(exit_status(255), 255);
        // Only the low 8 bits are kept.
        assert_eq!(exit_status(256), 0);
        assert_eq!(exit_status(0x1_0003), 3);
        assert_eq!(exit_status(!0), 255);
    }
}
