// SPDX-License-Identifier: MIT
//
// Copyright (c) 2026 Advanced Micro Devices, Inc
//
// Author: Mahmoud Nagy Adam <madam@amd.com>

/// First number of the range that is reserved for system calls that only the
/// SVSM has. Linux does not use numbers this high.
pub const NR_SVSM_BASE: u64 = 0x5356_0000;

/// The system calls of the SVSM, with the names and numbers of the Linux
/// x86-64 system calls they follow.
///
/// A system call that Linux does not have gets a number in the range that
/// starts at [`NR_SVSM_BASE`].
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysNo {
    Read = 0,
    Write = 1,
    Close = 3,
    Lseek = 8,
    Execve = 59,
    Ftruncate = 77,
    Getdents64 = 217,
    ExitGroup = 231,
    Openat = 257,
    Mkdirat = 258,
    Unlinkat = 263,
    /// Queries the capabilities of the SVSM. Linux has no such call.
    SvsmCapabilities = NR_SVSM_BASE,
}

impl TryFrom<u64> for SysNo {
    /// The number is not a system call of the SVSM.
    type Error = u64;

    fn try_from(nr: u64) -> Result<Self, u64> {
        Ok(match nr {
            0 => Self::Read,
            1 => Self::Write,
            3 => Self::Close,
            8 => Self::Lseek,
            59 => Self::Execve,
            77 => Self::Ftruncate,
            217 => Self::Getdents64,
            231 => Self::ExitGroup,
            257 => Self::Openat,
            258 => Self::Mkdirat,
            263 => Self::Unlinkat,
            NR_SVSM_BASE => Self::SvsmCapabilities,
            _ => return Err(nr),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sysno() {
        // The numbers are the Linux x86-64 ones.
        assert_eq!(SysNo::Read as u64, 0);
        assert_eq!(SysNo::Write as u64, 1);
        assert_eq!(SysNo::Close as u64, 3);
        assert_eq!(SysNo::Lseek as u64, 8);
        assert_eq!(SysNo::Execve as u64, 59);
        assert_eq!(SysNo::Ftruncate as u64, 77);
        assert_eq!(SysNo::Getdents64 as u64, 217);
        assert_eq!(SysNo::ExitGroup as u64, 231);
        assert_eq!(SysNo::Openat as u64, 257);
        assert_eq!(SysNo::Mkdirat as u64, 258);
        assert_eq!(SysNo::Unlinkat as u64, 263);
        assert_eq!(SysNo::SvsmCapabilities as u64, 0x5356_0000);
    }

    #[test]
    fn test_sysno_try_from() {
        let all = [
            SysNo::Read,
            SysNo::Write,
            SysNo::Close,
            SysNo::Lseek,
            SysNo::Execve,
            SysNo::Ftruncate,
            SysNo::Getdents64,
            SysNo::ExitGroup,
            SysNo::Openat,
            SysNo::Mkdirat,
            SysNo::Unlinkat,
            SysNo::SvsmCapabilities,
        ];
        for nr in all {
            assert_eq!(SysNo::try_from(nr as u64), Ok(nr));
        }
        // Numbers that are not system calls, next to valid ones too.
        for nr in [2, 4, 78, 256, 0x5356_0001, 0xdead_beef, u64::MAX] {
            assert_eq!(SysNo::try_from(nr), Err(nr));
        }
    }
}
