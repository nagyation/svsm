// SPDX-License-Identifier: MIT
//
// Copyright (c) 2024 SUSE LLC
//
// Author: Joerg Roedel <jroedel@suse.de>

use bitflags::bitflags;
use zerocopy::IntoBytes;

// Syscall classes
const CLASS0: u64 = 0;
const CLASS1: u64 = 1 << 32;
const CLASS3: u64 = 3 << 32;

// Syscall number in class0
pub const SYS_EXIT: u64 = CLASS0;
pub const SYS_EXEC: u64 = CLASS0 + 4;
pub const SYS_CLOSE: u64 = CLASS0 + 10;

// Syscall number in class1
pub const SYS_OPEN: u64 = CLASS1;
pub const SYS_READ: u64 = CLASS1 + 1;
pub const SYS_WRITE: u64 = CLASS1 + 2;
pub const SYS_SEEK: u64 = CLASS1 + 3;
pub const SYS_TRUNCATE: u64 = CLASS1 + 4;
pub const SYS_UNLINK: u64 = CLASS1 + 5;
pub const SYS_READDIR: u64 = CLASS1 + 7;
pub const SYS_MKDIR: u64 = CLASS1 + 8;

// Syscall number in class3
pub const SYS_CAPABILITIES: u64 = CLASS3;

// Flags of the open system call. They have the values of the Linux `O_*` flags
// on x86-64. Linux ignores the flags it does not know, and so does the SVSM.

/// Mask of the access mode in the flags
pub const O_ACCMODE: u32 = 0o3;
/// Open for reading only
pub const O_RDONLY: u32 = 0o0;
/// Open for writing only
pub const O_WRONLY: u32 = 0o1;
/// Open for reading and writing
pub const O_RDWR: u32 = 0o2;
/// Create the file if it does not exist
pub const O_CREAT: u32 = 0o100;
/// With `O_CREAT`, fail if the file exists
pub const O_EXCL: u32 = 0o200;
/// Truncate the file to zero
pub const O_TRUNC: u32 = 0o1000;
/// Every write goes to the end of the file
pub const O_APPEND: u32 = 0o2000;
/// Fail if the path is not a directory
pub const O_DIRECTORY: u32 = 0o200000;

/// Flag of the unlink system call to remove a directory
pub const AT_REMOVEDIR: u32 = 0x200;

/// Directory descriptor of the `*at()` system calls that stands for the
/// working directory of the task, which is its root directory
pub const AT_FDCWD: i32 = -100;

///Maximum length of path name including null character in bytes
pub const PATH_MAX: usize = 4096;

/// Maximum length of file name in bytes
pub const F_NAME_SIZE: usize = 256;

/// File descriptor of the standard input of a task, which is empty
pub const STDIN_FILENO: u32 = 0;

/// File descriptor of the standard output of a task, which is the log
pub const STDOUT_FILENO: u32 = 1;

/// File descriptor of the standard error of a task, which is the log
pub const STDERR_FILENO: u32 = 2;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, IntoBytes)]
pub enum FileType {
    File,
    Directory,
}

//
// Mode flags for Open system call
//
bitflags! {
    #[derive(Debug, Copy, Clone, Default)]
    pub struct FileModes: usize {
        /// Open file for reading
        const READ = 1 << 0;
        /// Open file for writing
        const WRITE = 1 << 1;
        /// Every write goes to the end of the file
        const APPEND = 1 << 2;
        /// Truncate file to zero
        const TRUNC = 1 << 3;
    }
}

//
// File flags for Open system call
//
bitflags! {
    #[derive(Debug, Copy, Clone, Default)]
    pub struct FileFlags: usize {
        /// Create file if it does not exist
        const CREATE = 1 << 0;
    }
}

//
// Modes for Seek system call
//
#[derive(Debug)]
pub enum SeekMode {
    /// Absolute file position
    Absolute = 0,
    /// Relative file position
    Relative = 1,
    /// File position relative to EOF
    End = 2,
}

impl From<SeekMode> for usize {
    fn from(mode: SeekMode) -> Self {
        mode as Self
    }
}

impl TryFrom<usize> for SeekMode {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value == SeekMode::Absolute.into() {
            Ok(SeekMode::Absolute)
        } else if value == SeekMode::Relative.into() {
            Ok(SeekMode::Relative)
        } else if value == SeekMode::End.into() {
            Ok(SeekMode::End)
        } else {
            Err(())
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, IntoBytes)]
pub struct DirEnt {
    /// Entry name
    pub file_name: [u8; F_NAME_SIZE],
    /// Entry type
    pub file_type: FileType,
    _pad: [u8; 7],
    /// File size - 0 for directories
    pub file_size: u64,
}

impl Default for DirEnt {
    fn default() -> Self {
        DirEnt {
            file_name: [b'\0'; F_NAME_SIZE],
            file_type: FileType::File,
            _pad: [0; 7],
            file_size: 0,
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct GlobalFeatureFlags: u64 {
        const _ = 0x7; // Define bits used for platform type since
                       // multi-bit flags should be avoided
    }
}

impl GlobalFeatureFlags {
    pub const PLATFORM_TYPE_NATIVE: u64 = 0;
    pub const PLATFORM_TYPE_SNP: u64 = 1;
    pub const PLATFORM_TYPE_TDP: u64 = 2;

    pub fn is_snp(&self) -> bool {
        (self.bits() & 0x7) == Self::PLATFORM_TYPE_SNP
    }

    pub fn is_tdp(&self) -> bool {
        (self.bits() & 0x7) == Self::PLATFORM_TYPE_TDP
    }
}

impl From<u64> for GlobalFeatureFlags {
    fn from(flags: u64) -> Self {
        GlobalFeatureFlags::from_bits_truncate(flags)
    }
}

impl From<GlobalFeatureFlags> for u64 {
    fn from(flags: GlobalFeatureFlags) -> Self {
        flags.bits() & GlobalFeatureFlags::all().bits()
    }
}
