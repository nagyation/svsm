// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2022-2023 SUSE LLC
//
// Author: Joerg Roedel <jroedel@suse.de>

use bitfield_struct::bitfield;

use crate::address::{Address, VirtAddr};
use crate::cpu::TlbFlushRange;
use crate::cpu::features::{Feature, cpu_get_feat};
use crate::cpu::tlb::TlbFlushScope;
use crate::types::PageSize;
use crate::utils::MemoryRegion;

use core::arch::asm;

#[bitfield(u64)]
struct InvlpgbRax {
    valid_va: bool,
    valid_pcid: bool,
    valid_asid: bool,
    global: bool,
    final_translation_only: bool,
    nested: bool,
    #[bits(6)]
    _rsvd: u8,
    #[bits(52)]
    va: usize,
}

#[bitfield(u32)]
struct InvlpgbEcx {
    count: u16,
    #[bits(15)]
    _rsvd: u16,
    huge: bool,
}

#[inline]
fn do_invlpgb(rax: u64, rcx: u64, rdx: u64) {
    // SAFETY: Inline assembly to invalidate TLB Entries, which does not change
    // any state related to memory safety.
    unsafe {
        asm!("invlpgb",
             in("rax") rax,
             in("rcx") rcx,
             in("rdx") rdx,
             options(att_syntax, nostack, preserves_flags));
    }
}

#[inline]
fn do_tlbsync() {
    // SAFETY: Inline assembly to synchronize TLB invalidations. It does not
    // change any state.
    unsafe {
        asm!("tlbsync", options(att_syntax, nomem, preserves_flags));
    }
}

fn flush_tlb(global: bool) {
    let rax = InvlpgbRax::new().with_valid_asid(true).with_global(global);
    do_invlpgb(rax.into_bits(), 0, 0);
}

fn flush_tlb_sync(global: bool) {
    flush_tlb(global);
    do_tlbsync();
}

/// Splits a region into the batches that one INVLPGB instruction flushes.
///
/// Returns the start address and the number of pages of each batch, which has
/// at most `max_count` pages.
///
/// The length of the region is the difference of its addresses and not of their
/// raw values. A region can end at `USER_MEM_END`, as the user stack does,
/// which is stored sign-extended and so looks to be far away from the start of
/// the region.
fn invlpgb_batches(
    region: MemoryRegion<VirtAddr>,
    pgsize: PageSize,
    max_count: usize,
) -> impl Iterator<Item = (VirtAddr, usize)> {
    let start = region.start();
    let size = usize::from(pgsize);
    let pages = (region.end() - start).div_ceil(size);

    (0..pages).step_by(max_count).map(move |first| {
        let count = (pages - first).min(max_count);
        (start + first * size, count)
    })
}

fn flush_tlb_sync_range(global: bool, region: MemoryRegion<VirtAddr>, pgsize: PageSize) {
    // The CPU reports the largest number of pages of one instruction minus
    // one, as the instruction does for its count.
    let max_count = cpu_get_feat(Feature::InvlpgbMax) as usize + 1;

    for (start, count) in invlpgb_batches(region, pgsize, max_count) {
        let ecx = InvlpgbEcx::new()
            // The count of the instruction is the number of additional pages,
            // which is not more than what the CPU reports for the maximum.
            .with_count((count - 1) as u16)
            .with_huge(pgsize == PageSize::Huge);

        let rax = InvlpgbRax::new()
            .with_valid_asid(true)
            .with_global(global)
            .with_valid_va(true)
            .with_va(start.pfn());

        do_invlpgb(rax.into_bits(), ecx.into_bits() as u64, 0);
    }

    do_tlbsync();
}

fn flush_address(va: VirtAddr) {
    let rax = InvlpgbRax::new()
        .with_valid_asid(true)
        .with_global(true)
        .with_valid_va(true)
        .with_va(va.pfn());
    do_invlpgb(rax.into_bits(), 0, 0);
}

pub fn flush_address_sync(va: VirtAddr) {
    flush_address(va);
    do_tlbsync();
}

pub fn flush_tlb_scope(flush_scope: &TlbFlushScope) {
    match flush_scope.range {
        TlbFlushRange::All => flush_tlb_sync(flush_scope.global),
        TlbFlushRange::Range { region, pgsize } => {
            flush_tlb_sync_range(flush_scope.global, region, pgsize)
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::mm::USER_MEM_END;
    use crate::types::PAGE_SIZE;
    use alloc::vec::Vec;

    #[test]
    fn test_invlpgb_batches() {
        let start = VirtAddr::from(0x10_0000usize);
        let region = MemoryRegion::new(start, 10 * PAGE_SIZE);

        let batches: Vec<_> = invlpgb_batches(region, PageSize::Regular, 4).collect();
        assert_eq!(
            batches,
            [
                (start, 4),
                (start + 4 * PAGE_SIZE, 4),
                (start + 8 * PAGE_SIZE, 2)
            ]
        );

        // The whole region fits a single batch.
        let batches: Vec<_> = invlpgb_batches(region, PageSize::Regular, 10).collect();
        assert_eq!(batches, [(start, 10)]);
    }

    #[test]
    fn test_invlpgb_batches_end_of_user_space() {
        // A region that ends at USER_MEM_END is not as long as the difference
        // of the raw addresses.
        let start = USER_MEM_END - 16 * PAGE_SIZE;
        let region = MemoryRegion::from_addresses(start, USER_MEM_END);

        let batches: Vec<_> = invlpgb_batches(region, PageSize::Regular, 0x10000).collect();
        assert_eq!(batches, [(start, 16)]);

        let batches: Vec<_> = invlpgb_batches(region, PageSize::Regular, 8).collect();
        assert_eq!(batches, [(start, 8), (start + 8 * PAGE_SIZE, 8)]);
    }
}
