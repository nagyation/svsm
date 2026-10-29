// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2023 SUSE LLC
//
// Author: Joerg Roedel <jroedel@suse.de>

use super::*;

use crate::error::SvsmError;
use crate::locking::RWLock;
use crate::mm::PageRef;
use crate::types::{PAGE_SHIFT, PAGE_SIZE};
use crate::utils::{page_align_up, page_offset};

extern crate alloc;
use alloc::sync::Arc;
use alloc::vec::Vec;

use core::cmp::{max, min};

/// The largest size of a file. The files live in memory, so this keeps one
/// of them from using all of it.
pub const MAX_FILE_SIZE: usize = 64 * 1024 * 1024;

/// Represents an SVSM Ramfile
#[derive(Debug, Default)]
struct RawRamFile {
    /// Maximum size of the file without allocating new pages
    capacity: usize,
    /// Current size of the file
    size: usize,
    /// Vector of pages allocated for the file
    pages: Vec<PageRef>,
}

impl RawRamFile {
    /// Used to get new instance of [`RawRamFile`].
    pub fn new() -> Self {
        RawRamFile {
            capacity: 0,
            size: 0,
            pages: Vec::new(),
        }
    }

    /// Used to increase the capacity of the file by allocating a
    /// new page.
    ///
    /// # Returns
    ///
    /// [`Result<(), SvsmError>`]: A [`Result`] containing empty
    /// value if successful, SvsvError otherwise.
    fn increase_capacity(&mut self) -> Result<(), SvsmError> {
        let page_ref = PageRef::new()?;
        self.pages.push(page_ref);
        self.capacity += PAGE_SIZE;
        Ok(())
    }

    /// Used to set the capacity of the file.
    ///
    /// # Argument
    ///
    /// `capacity`: intended new capacity of the file.
    ///
    /// # Returns
    ///
    /// [`Result<(), SvsmError>`]: A [Result] containing empty
    /// value if successful, SvsmError otherwise.
    fn set_capacity(&mut self, capacity: usize) -> Result<(), SvsmError> {
        let cap = page_align_up(capacity);

        while cap > self.capacity {
            self.increase_capacity()?;
        }

        Ok(())
    }

    /// Sets the capacity of the file like [`Self::set_capacity()`], and frees
    /// the pages it allocated again if that fails.
    fn grow_capacity(&mut self, capacity: usize) -> Result<(), SvsmError> {
        let pages = self.pages.len();
        self.set_capacity(capacity).inspect_err(|_| {
            self.pages.truncate(pages);
            self.capacity = pages * PAGE_SIZE;
        })
    }

    /// Read data from a file page and store it in a Buffer object.
    ///
    /// # Arguments:
    ///
    /// - `buffer`: [`Buffer`] object to store read data.
    /// - `buffer_offset`: Offset into the buffer.
    /// - `file_offset`: Offset into the file.
    ///
    /// # Returns:
    ///
    /// [`Result`] with number of bytes read on success, [`SvsmError`] on
    /// failure.
    #[inline(always)]
    fn read_buffer_from_page(
        &self,
        buffer: &mut dyn Buffer,
        buffer_offset: usize,
        file_offset: usize,
    ) -> Result<usize, SvsmError> {
        let page_offset = page_offset(file_offset);
        let page_index = file_offset / PAGE_SIZE;

        // Make sure the page exists before trying to read from it.
        let Some(page) = self.pages.get(page_index) else {
            return Ok(0);
        };

        // Minimum of space bytes-to-read on the page and remaining space in buffer
        let buffer_min = min(buffer.size() - buffer_offset, PAGE_SIZE - page_offset);
        // Make sure to not read beyond EOF
        let size = min(self.size.checked_sub(file_offset).unwrap(), buffer_min);

        page.copy_to_buffer(buffer, buffer_offset, page_offset, size)
    }

    /// Write data from [`Buffer`] object to a file page.
    ///
    /// # Arguments:
    ///
    /// - `buffer`: [`Buffer`] object to read data from.
    /// - `buffer_offset`: Offset into the buffer.
    /// - `file_offset`: Offset into the file.
    ///
    /// # Returns:
    ///
    /// [`Result`] with number of bytes written on success, [`SvsmError`] on
    /// failure.
    #[inline(always)]
    fn write_buffer_to_page(
        &self,
        buffer: &dyn Buffer,
        buffer_offset: usize,
        file_offset: usize,
    ) -> Result<usize, SvsmError> {
        let page_offset = page_offset(file_offset);
        let page_index = file_offset / PAGE_SIZE;

        // Minimum of space on the page and remaining bytes-to-write in buffer
        let size = min(
            buffer.size().checked_sub(buffer_offset).unwrap(),
            PAGE_SIZE - page_offset,
        );

        self.pages[page_index].copy_from_buffer(buffer, buffer_offset, page_offset, size)
    }

    /// Used to read the file from a particular offset.
    ///
    /// # Arguments
    ///
    /// - `buf`: buffer to read the contents of the file into.
    /// - `file_offset`: file offset to read from.
    ///
    /// # Returns
    ///
    /// [`Result<(), SvsmError>`]: A [Result] containing empty
    /// value if successful, SvsmError otherwise.
    fn read(&self, buf: &mut [u8], file_offset: usize) -> Result<usize, SvsmError> {
        self.read_buffer(&mut SliceMutRefBuffer::new(buf), file_offset)
    }

    fn read_buffer(&self, buffer: &mut dyn Buffer, file_offset: usize) -> Result<usize, SvsmError> {
        let mut current = min(file_offset, self.size);
        let mut len = buffer.size();
        let mut buffer_offset: usize = 0;

        while len > 0 {
            let read = self.read_buffer_from_page(buffer, buffer_offset, current)?;
            current += read;
            buffer_offset += read;
            len -= read;
            if current == self.size {
                break;
            }
        }

        Ok(buffer_offset)
    }

    /// Used to write to the file at a particular offset.
    ///
    /// # Arguments
    ///
    /// - `buf`: buffer that contains the data to write into the file.
    /// - `file_offset`: file offset to read from.
    ///
    /// # Returns
    ///
    /// [`Result<(), SvsmError>`]: A [Result] containing empty
    /// value if successful, SvsmError otherwise.
    fn write(&mut self, buf: &[u8], file_offset: usize) -> Result<usize, SvsmError> {
        self.write_buffer(&SliceRefBuffer::new(buf), file_offset)
    }

    fn write_buffer(
        &mut self,
        buffer: &dyn Buffer,
        file_offset: usize,
    ) -> Result<usize, SvsmError> {
        // An empty write does not need any memory, wherever it is.
        if buffer.size() == 0 {
            return Ok(0);
        }

        // Like on Linux, a write that starts at the largest size fails, and
        // one that crosses it is shortened.
        if file_offset >= MAX_FILE_SIZE {
            return Err(SvsmError::FileSystem(FsError::file_too_big()));
        }

        let mut current = file_offset;
        let mut len = buffer.size().min(MAX_FILE_SIZE - file_offset);
        let mut buffer_offset: usize = 0;

        self.grow_capacity(file_offset + len)?;

        while len > 0 {
            let written = self.write_buffer_to_page(buffer, buffer_offset, current)?;
            current += written;
            buffer_offset += written;
            len -= written;
            self.size = max(self.size, current);
        }

        Ok(buffer_offset)
    }

    /// Used to truncate the file to a given size. A size larger than the
    /// current one extends the file with zeros.
    ///
    /// # Argument
    ///
    /// - `size` : intended file size after truncation
    ///
    /// # Returns
    ///
    /// [`Result<usize, SvsmError>`]: a [`Result`] containing the
    /// number of bytes file truncated to if successful, SvsmError
    /// otherwise.
    fn truncate(&mut self, size: usize) -> Result<usize, SvsmError> {
        if size > MAX_FILE_SIZE {
            return Err(SvsmError::FileSystem(FsError::file_too_big()));
        }

        if size > self.size {
            // The pages of a file are zeroed, and so is the part of the last
            // page after the end of the file.
            self.grow_capacity(size)?;
            self.size = size;
            return Ok(size);
        }

        let offset = page_offset(size);
        let base_pages = size / PAGE_SIZE;
        let new_pages = if offset > 0 {
            base_pages + 1
        } else {
            base_pages
        };

        // Clear pages and remove them from the file
        for page_ref in self.pages.drain(new_pages..) {
            page_ref.fill(0, 0);
        }

        self.capacity = new_pages * PAGE_SIZE;
        self.size = size;

        if offset > 0 {
            // Clear the last page after new EOF
            let page_ref = self.pages.last().unwrap();
            page_ref.fill(offset, 0);
        }

        Ok(size)
    }

    /// Used to get the size of the file in bytes.
    ///
    /// # Returns
    /// the size of the file in bytes.
    fn size(&self) -> usize {
        self.size
    }

    fn mapping(&self, offset: usize) -> Option<PageRef> {
        if offset > self.size() {
            return None;
        }
        self.pages.get(offset >> PAGE_SHIFT).cloned()
    }
}

/// Represents a SVSM file with synchronized access
#[derive(Debug)]
pub struct RamFile {
    rawfile: RWLock<RawRamFile>,
}

impl RamFile {
    /// Used to get a new instance of [`RamFile`].
    pub fn new() -> Self {
        RamFile {
            rawfile: RWLock::new(RawRamFile::new()),
        }
    }
}

impl File for RamFile {
    fn read(&self, buf: &mut [u8], offset: usize) -> Result<usize, SvsmError> {
        self.rawfile.lock_read().read(buf, offset)
    }

    fn read_buffer(&self, buffer: &mut dyn Buffer, file_offset: usize) -> Result<usize, SvsmError> {
        self.rawfile.lock_read().read_buffer(buffer, file_offset)
    }

    fn write(&self, buf: &[u8], offset: usize) -> Result<usize, SvsmError> {
        self.rawfile.lock_write().write(buf, offset)
    }

    fn write_buffer(&self, buffer: &dyn Buffer, file_offset: usize) -> Result<usize, SvsmError> {
        self.rawfile.lock_write().write_buffer(buffer, file_offset)
    }

    fn truncate(&self, size: usize) -> Result<usize, SvsmError> {
        self.rawfile.lock_write().truncate(size)
    }

    fn size(&self) -> usize {
        self.rawfile.lock_read().size()
    }

    fn mapping(&self, offset: usize) -> Option<PageRef> {
        self.rawfile.lock_read().mapping(offset)
    }
}

#[derive(Debug)]
struct RawRamDirectory {
    entries: Vec<DirectoryEntry>,
    remove_in_progress: bool,
}

impl RawRamDirectory {
    /// Used to get a new instance of [`RamDirectory`]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            remove_in_progress: false,
        }
    }

    /// Used to check if an entry is present in the directory.
    ///
    ///  # Argument
    ///
    ///  `name`: name of the entry to be looked up.
    ///
    ///  # Returns
    ///  [`true`] if the entry is present, [`false`] otherwise.
    fn has_entry(&self, name: &FileName) -> bool {
        self.entries.iter().any(|entry| entry.name == *name)
    }

    fn check_remove(&self) -> Result<(), SvsmError> {
        if self.remove_in_progress {
            Err(SvsmError::FileSystem(FsError::busy()))
        } else {
            Ok(())
        }
    }

    fn list(&self) -> Vec<FileName> {
        self.entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    }

    fn prepare_remove(&mut self) -> Result<(), SvsmError> {
        self.check_remove()?;
        // Only report success if directory is empty.
        if self.entries.is_empty() {
            self.remove_in_progress = true;
            Ok(())
        } else {
            Err(SvsmError::FileSystem(FsError::not_empty()))
        }
    }

    fn lookup_entry(&self, name: FileNameRef<'_>) -> Result<DirEntry, SvsmError> {
        for e in self.entries.iter() {
            if e.name == name {
                return Ok(e.entry.clone());
            }
        }

        Err(SvsmError::FileSystem(FsError::file_not_found()))
    }

    fn create_file(&mut self, name: FileName) -> Result<Arc<dyn File>, SvsmError> {
        self.check_remove()?;

        if self.has_entry(&name) {
            return Err(SvsmError::FileSystem(FsError::file_exists()));
        }

        let new_file = Arc::new(RamFile::new());
        self.entries
            .push(DirectoryEntry::new(name, DirEntry::File(new_file.clone())));

        Ok(new_file)
    }

    fn create_directory(&mut self, name: FileName) -> Result<Arc<dyn Directory>, SvsmError> {
        self.check_remove()?;

        if self.has_entry(&name) {
            return Err(SvsmError::FileSystem(FsError::file_exists()));
        }

        let new_dir = Arc::new(RamDirectory::new());
        self.entries.push(DirectoryEntry::new(
            name,
            DirEntry::Directory(new_dir.clone()),
        ));

        Ok(new_dir)
    }

    fn unlink(&mut self, name: FileNameRef<'_>) -> Result<(), SvsmError> {
        let pos = self.entries.iter().position(|e| e.name == name);
        match pos {
            Some(idx) => {
                self.entries.swap_remove(idx);
                Ok(())
            }
            None => Err(SvsmError::FileSystem(FsError::file_not_found())),
        }
    }
}

/// Represents a SVSM directory with synchronized access
#[derive(Debug)]
pub struct RamDirectory {
    directory: RWLock<RawRamDirectory>,
}

impl RamDirectory {
    /// Used to get a new instance of [`RamDirectory`]
    pub fn new() -> Self {
        RamDirectory {
            directory: RWLock::new(RawRamDirectory::new()),
        }
    }
}

/// Checks that a name is not longer than [`NAME_MAX`].
fn check_name(name: FileNameRef<'_>) -> Result<(), SvsmError> {
    if name.len() > NAME_MAX {
        return Err(SvsmError::FileSystem(FsError::name_too_long()));
    }
    Ok(())
}

impl Directory for RamDirectory {
    fn list(&self) -> Vec<FileName> {
        self.directory.lock_read().list()
    }

    fn prepare_remove(&self) -> Result<(), SvsmError> {
        self.directory.lock_write().prepare_remove()
    }

    fn lookup_entry(&self, name: FileNameRef<'_>) -> Result<DirEntry, SvsmError> {
        check_name(name)?;
        self.directory.lock_read().lookup_entry(name)
    }

    fn create_file(&self, name: FileName) -> Result<Arc<dyn File>, SvsmError> {
        check_name(&name)?;
        self.directory.lock_write().create_file(name)
    }

    fn create_directory(&self, name: FileName) -> Result<Arc<dyn Directory>, SvsmError> {
        check_name(&name)?;
        self.directory.lock_write().create_directory(name)
    }

    fn unlink(&self, name: FileNameRef<'_>) -> Result<(), SvsmError> {
        self.directory.lock_write().unlink(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::alloc::{DEFAULT_TEST_MEMORY_SIZE, TestRootMem};

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_ramfs_file_read_write() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        let mut buf1 = [0xffu8; 512];

        // Write first buffer at offset 0
        file.write(&buf1, 0).expect("Failed to write file data");
        assert!(file.size() == 512);

        // Write second buffer at offset 4096 - 256 - cross-page write
        let mut buf2 = [0xaau8; 512];
        file.write(&buf2, PAGE_SIZE - 256)
            .expect("Failed to write file cross-page");
        assert!(file.size() == PAGE_SIZE + 256);

        // Clear buffer before reading into it
        buf1 = [0u8; 512];

        // Read back and check first buffer
        let size = file
            .read(&mut buf1, 0)
            .expect("Failed to read from offset 0");
        assert!(size == 512);

        for byte in buf1.iter() {
            assert!(*byte == 0xff);
        }

        // Clear buffer before reading into it
        buf2 = [0u8; 512];

        // Read back and check second buffer
        let size = file
            .read(&mut buf2, PAGE_SIZE - 256)
            .expect("Failed to read from offset PAGE_SIZE - 256");
        assert!(size == 512);

        for byte in buf2.iter() {
            assert!(*byte == 0xaa);
        }

        // Check complete file
        let mut buf3: [u8; 8192] = [0xcc; 8192];
        let size = file.read(&mut buf3, 0).expect("Failed to read whole file");
        assert!(size == PAGE_SIZE + 256);

        for (i, elem) in buf3.iter().enumerate() {
            let expected: u8 = if i < 512 {
                0xff
            } else if i < PAGE_SIZE - 256 {
                0
            } else if i < PAGE_SIZE + 256 {
                0xaa
            } else {
                0xcc
            };
            assert!(*elem == expected);
        }

        assert_eq!(file.truncate(1024).unwrap(), 1024);
        assert_eq!(file.size(), 1024);

        // Clear buffer before reading again into it
        buf3 = [0u8; 8192];

        // read file again
        let size = file.read(&mut buf3, 0).expect("Failed to read whole file");
        assert!(size == 1024);

        for (i, elem) in buf3.iter().enumerate().take(1024) {
            let expected: u8 = if i < 512 { 0xff } else { 0 };
            assert!(*elem == expected);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_ramfs_file_extend() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        file.write(&[0xffu8; 100], 0).unwrap();

        // Extending the file past a page boundary adds zeros.
        assert_eq!(file.truncate(PAGE_SIZE + 10).unwrap(), PAGE_SIZE + 10);
        assert_eq!(file.size(), PAGE_SIZE + 10);
        let mut buf = [0xccu8; PAGE_SIZE + 10];
        assert_eq!(file.read(&mut buf, 0).unwrap(), PAGE_SIZE + 10);
        for (i, byte) in buf.iter().enumerate() {
            assert_eq!(*byte, if i < 100 { 0xff } else { 0 });
        }

        // Shrinking and extending again does not bring the old data back.
        file.truncate(50).unwrap();
        file.truncate(200).unwrap();
        let mut buf = [0xccu8; 200];
        assert_eq!(file.read(&mut buf, 0).unwrap(), 200);
        for (i, byte) in buf.iter().enumerate() {
            assert_eq!(*byte, if i < 50 { 0xff } else { 0 });
        }

        // A size that is too large is refused and the file is unchanged.
        file.truncate(usize::MAX).unwrap_err();
        assert_eq!(file.size(), 200);
    }

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_ramfs_file_size_limit() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();

        // A write at the largest size fails without allocating anything,
        // and an empty one does nothing wherever it is.
        let err = file.write(b"x", MAX_FILE_SIZE).unwrap_err();
        assert!(matches!(err, SvsmError::FileSystem(FsError::FileTooBig)));
        assert_eq!(file.write(b"", MAX_FILE_SIZE).unwrap(), 0);
        assert_eq!(file.size(), 0);

        // So does extending the file past the largest size.
        let err = file.truncate(MAX_FILE_SIZE + 1).unwrap_err();
        assert!(matches!(err, SvsmError::FileSystem(FsError::FileTooBig)));
        assert_eq!(file.size(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_ramfs_file_out_of_memory() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();

        // A write that needs more memory than there is, and an extension of
        // the file too, fail and leave the file as it was.
        file.write(b"a", 2 * DEFAULT_TEST_MEMORY_SIZE).unwrap_err();
        file.truncate(2 * DEFAULT_TEST_MEMORY_SIZE).unwrap_err();
        assert_eq!(file.size(), 0);

        // The pages the failed calls allocated were freed: a write that needs
        // the first pages again works, and reads back.
        file.write(b"b", 3 * PAGE_SIZE).unwrap();
        assert_eq!(file.size(), 3 * PAGE_SIZE + 1);
        let mut buf = [0xffu8; 1];
        assert_eq!(file.read(&mut buf, 3 * PAGE_SIZE).unwrap(), 1);
        assert_eq!(&buf, b"b");
    }

    #[test]
    #[cfg_attr(miri, ignore = "inline assembly")]
    fn test_ram_directory_name_length() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let ram_dir = RamDirectory::new();
        let longest = "a".repeat(NAME_MAX);
        let too_long = "a".repeat(NAME_MAX + 1);

        // A name of the largest length works.
        ram_dir.create_file(longest.clone()).unwrap();
        assert!(ram_dir.lookup_entry(&longest).unwrap().is_file());
        ram_dir.create_directory("b".repeat(NAME_MAX)).unwrap();

        // A longer name can neither be created nor looked up.
        let err = ram_dir.create_file(too_long.clone()).unwrap_err();
        assert!(matches!(err, SvsmError::FileSystem(FsError::NameTooLong)));
        let err = ram_dir.create_directory(too_long.clone()).unwrap_err();
        assert!(matches!(err, SvsmError::FileSystem(FsError::NameTooLong)));
        let err = ram_dir.lookup_entry(&too_long).err().unwrap();
        assert!(matches!(err, SvsmError::FileSystem(FsError::NameTooLong)));
    }

    #[test]
    fn test_ram_directory() {
        let f_name = FileName::from("file1");
        let d_name = FileName::from("dir1");

        let ram_dir = RamDirectory::new();

        ram_dir
            .create_file(f_name.clone())
            .expect("Failed to create file");
        ram_dir
            .create_directory(d_name.clone())
            .expect("Failed to create directory");

        let list = ram_dir.list();
        assert_eq!(list, [f_name.as_str(), d_name.as_str()]);

        let entry = ram_dir
            .lookup_entry(&f_name)
            .expect("Failed to lookup file");
        assert!(entry.is_file());

        let entry = ram_dir
            .lookup_entry(&d_name)
            .expect("Failed to lookup directory");
        assert!(entry.is_dir());

        ram_dir.unlink(&d_name).expect("Failed to unlink directory");

        let list = ram_dir.list();
        assert_eq!(list, [f_name]);
    }

    #[test]
    fn test_ramfs_single_page_mapping() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        let buf = [0xffu8; 512];

        file.write(&buf, 0).expect("Failed to write file data");

        let res = file
            .mapping(0)
            .expect("Failed to get mapping for ramfs page");
        assert_eq!(
            res.phys_addr(),
            file.rawfile.lock_read().pages[0].phys_addr()
        );
    }

    #[test]
    fn test_ramfs_multi_page_mapping() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        let buf = [0xffu8; 4 * PAGE_SIZE];

        file.write(&buf, 0).expect("Failed to write file data");

        for i in 0..4 {
            let res = file
                .mapping(i * PAGE_SIZE)
                .expect("Failed to get mapping for ramfs page");
            assert_eq!(
                res.phys_addr(),
                file.rawfile.lock_read().pages[i].phys_addr()
            );
        }
    }

    #[test]
    fn test_ramfs_mapping_unaligned_offset() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        let buf = [0xffu8; 4 * PAGE_SIZE];

        file.write(&buf, 0).expect("Failed to write file data");

        let res = file
            .mapping(PAGE_SIZE + 0x123)
            .expect("Failed to get mapping for ramfs page");
        assert_eq!(
            res.phys_addr(),
            file.rawfile.lock_read().pages[1].phys_addr()
        );
    }

    #[test]
    fn test_ramfs_mapping_out_of_range() {
        let _test_mem = TestRootMem::setup(DEFAULT_TEST_MEMORY_SIZE);

        let file = RamFile::new();
        let buf = [0xffu8; 4 * PAGE_SIZE];

        file.write(&buf, 0).expect("Failed to write file data");

        let res = file.mapping(4 * PAGE_SIZE);
        assert!(res.is_none());
    }
}
