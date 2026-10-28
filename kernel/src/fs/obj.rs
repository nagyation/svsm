// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2024 Intel Corporation.
//
// Author: Peter Fang <peter.fang@intel.com>

extern crate alloc;

use super::filesystem::seek_target;
use super::{Buffer, DirEntry, Directory, FileHandle, FileName, FsError};
use crate::error::SvsmError;
use crate::syscall::Obj;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use syscall::SeekMode;

#[derive(Debug)]
struct DirectoryHandle {
    dir: Arc<dyn Directory>,
    list: Vec<FileName>,
    next: AtomicUsize,
}

impl DirectoryHandle {
    fn new(dir: &Arc<dyn Directory>) -> Self {
        DirectoryHandle {
            dir: dir.clone(),
            list: dir.list(),
            next: AtomicUsize::new(0),
        }
    }
}

#[derive(Debug)]
enum FsObjEntry {
    File(FileHandle),
    Directory(DirectoryHandle),
}

#[derive(Debug)]
pub struct FsObj {
    entry: FsObjEntry,
}

impl FsObj {
    pub fn new_dir(dir: &Arc<dyn Directory>) -> Self {
        FsObj {
            entry: FsObjEntry::Directory(DirectoryHandle::new(dir)),
        }
    }

    pub fn new_file(file_handle: FileHandle) -> Self {
        Self {
            entry: FsObjEntry::File(file_handle),
        }
    }

    pub fn read_buffer(&self, buffer: &mut dyn Buffer) -> Result<usize, SvsmError> {
        let FsObjEntry::File(fh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        fh.read_buffer(buffer)
    }

    pub fn write_buffer(&self, buffer: &dyn Buffer) -> Result<usize, SvsmError> {
        let FsObjEntry::File(fh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        fh.write_buffer(buffer)
    }

    /// Changes the file offset like the Linux lseek() and returns it.
    ///
    /// The offset of a directory is the position of the next entry. It can be
    /// set to the start of the directory or relative to the current
    /// position, and stops after the last entry.
    pub fn seek(&self, mode: SeekMode, offset: i64) -> Result<usize, SvsmError> {
        match &self.entry {
            FsObjEntry::File(fh) => fh.seek(mode, offset),
            FsObjEntry::Directory(dh) => {
                let base = match mode {
                    SeekMode::Absolute => 0,
                    SeekMode::Relative => dh.next.load(Ordering::Relaxed),
                    // The end of a directory has no meaning.
                    SeekMode::End => return Err(SvsmError::FileSystem(FsError::inval())),
                };
                self.dir_seek(seek_target(base, offset)?)
            }
        }
    }

    pub fn truncate(&self, length: usize) -> Result<usize, SvsmError> {
        let FsObjEntry::File(fh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        fh.truncate(length)
    }

    /// Returns the directory this object refers to.
    ///
    /// # Returns
    ///
    /// The directory, or `None` if this object refers to a file.
    pub fn directory(&self) -> Option<Arc<dyn Directory>> {
        match &self.entry {
            FsObjEntry::Directory(dh) => Some(dh.dir.clone()),
            FsObjEntry::File(_) => None,
        }
    }

    /// Returns the position of the directory entry [`FsObj::readdir()`]
    /// returns next.
    ///
    /// # Returns
    ///
    /// The position on success, `Err(SvsmError::NotSupported)` if this
    /// object refers to a file.
    pub fn dir_position(&self) -> Result<usize, SvsmError> {
        let FsObjEntry::Directory(dh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        Ok(dh.next.load(Ordering::Relaxed))
    }

    /// Sets the position of the directory entry [`FsObj::readdir()`]
    /// returns next. Positions after the last entry are reduced to the
    /// position after the last entry.
    ///
    /// # Arguments
    ///
    /// * `pos` - The new position.
    ///
    /// # Returns
    ///
    /// The new position on success, `Err(SvsmError::NotSupported)` if this
    /// object refers to a file.
    pub fn dir_seek(&self, pos: usize) -> Result<usize, SvsmError> {
        let FsObjEntry::Directory(dh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        let pos = pos.min(dh.list.len());
        dh.next.store(pos, Ordering::Relaxed);
        Ok(pos)
    }

    /// Returns the directory entry at a given position without changing the
    /// position of the object.
    ///
    /// The entries are listed when the object is created. An entry that was
    /// removed from the directory since then is reported as not found.
    ///
    /// # Arguments
    ///
    /// * `pos` - The position of the entry.
    ///
    /// # Returns
    ///
    /// The name and entry at `pos`, `Ok(None)` if `pos` is after the last
    /// entry, or an [`SvsmError`] on failure.
    pub fn dir_entry_at(&self, pos: usize) -> Result<Option<(FileName, DirEntry)>, SvsmError> {
        let FsObjEntry::Directory(dh) = &self.entry else {
            return Err(SvsmError::NotSupported);
        };

        let Some(name) = dh.list.get(pos) else {
            return Ok(None);
        };
        let dirent = dh.dir.lookup_entry(name)?;
        Ok(Some((name.clone(), dirent)))
    }
}

impl Obj for FsObj {
    fn as_fs(&self) -> Option<&FsObj> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{
        SliceMutRefBuffer, SliceRefBuffer, TestFileSystemGuard, create, mkdir, open_rw, opendir,
        rmdir, unlink,
    };
    use crate::mm::alloc::{DEFAULT_TEST_MEMORY_SIZE, TestRootMem};

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_fsobj_append() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);
        let _test_fs = TestFileSystemGuard::setup();

        create("fsobj_append").unwrap();
        let obj = FsObj::new_file(open_rw("fsobj_append").unwrap().with_append());
        obj.write_buffer(&SliceRefBuffer::new(b"ab")).unwrap();

        // A write after seeking back still appends.
        obj.seek(SeekMode::Absolute, 0).unwrap();
        obj.write_buffer(&SliceRefBuffer::new(b"cd")).unwrap();
        assert_eq!(obj.seek(SeekMode::End, 0).unwrap(), 4);

        let mut buf = [0u8; 4];
        obj.seek(SeekMode::Absolute, 0).unwrap();
        obj.read_buffer(&mut SliceMutRefBuffer::new(&mut buf))
            .unwrap();
        assert_eq!(&buf, b"abcd");

        // Appends through other handles do not overwrite each other, even if
        // the offset of the handle is somewhere else.
        let other = open_rw("fsobj_append").unwrap().with_append();
        assert_eq!(other.write(b"ef").unwrap(), 2);
        obj.write_buffer(&SliceRefBuffer::new(b"gh")).unwrap();
        assert_eq!(other.write(b"ij").unwrap(), 2);
        assert_eq!(obj.seek(SeekMode::End, 0).unwrap(), 10);

        let mut buf = [0u8; 10];
        obj.seek(SeekMode::Absolute, 0).unwrap();
        obj.read_buffer(&mut SliceMutRefBuffer::new(&mut buf))
            .unwrap();
        assert_eq!(&buf, b"abcdefghij");

        // Cleanup
        unlink("fsobj_append").unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_fsobj_dir_position() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);
        let _test_fs = TestFileSystemGuard::setup();

        mkdir("fsobj_dir").unwrap();
        create("fsobj_dir/file1").unwrap();
        create("fsobj_dir/file2").unwrap();
        let obj = FsObj::new_dir(&opendir("fsobj_dir").unwrap());
        assert!(obj.directory().is_some());

        // Reading an entry at a position does not move the position.
        let (name, _) = obj.dir_entry_at(1).unwrap().unwrap();
        assert_eq!(name, "file2");
        assert!(obj.dir_entry_at(2).unwrap().is_none());
        assert_eq!(obj.dir_position().unwrap(), 0);

        // The position is the entry that is read next.
        assert_eq!(obj.dir_seek(1).unwrap(), 1);
        assert_eq!(obj.dir_position().unwrap(), 1);
        let (name, _) = obj.dir_entry_at(1).unwrap().unwrap();
        assert_eq!(name, "file2");

        // Positions are limited to the number of entries.
        assert_eq!(obj.dir_seek(10).unwrap(), 2);

        // Entries removed after the listing are not found.
        unlink("fsobj_dir/file1").unwrap();
        obj.dir_entry_at(0).unwrap_err();

        // Files have no directory.
        let file = FsObj::new_file(open_rw("fsobj_dir/file2").unwrap());
        assert!(file.directory().is_none());
        file.dir_position().unwrap_err();

        // Cleanup
        unlink("fsobj_dir/file2").unwrap();
        rmdir("fsobj_dir").unwrap();
    }
}
