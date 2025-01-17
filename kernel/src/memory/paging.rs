use core::cell::RefCell;
use core::fmt::{Debug, Formatter};
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use kernel_api::memory::{Frame, Page, PhysicalAddress, VirtualAddress, AllocError};
use kernel_api::memory::allocator::{BackingAllocator};
use kernel_api::sync::RwLock;

use crate::hal::paging::{Table, PageIndices, levels::Global, Entry, TableDebug};
use crate::hal::paging2::{KTable, KTableTy};
use crate::hal::paging::levels::ParentLevel;
use crate::sync::late_init::LateInit;

static KERNEL_PAGE_TABLE: LateInit<RwLock<KTableTy>> = LateInit::new();

pub unsafe fn init_page_table(active_page_table: KTableTy) {
	KERNEL_PAGE_TABLE.init_ref(RwLock::new(active_page_table));
}

#[export_name = "__popcorn_paging_get_ktable"]
pub fn ktable() -> impl DerefMut<Target = KTableTy> {
	KERNEL_PAGE_TABLE.write()
}

#[cfg(test)]
mod tests {
	use crate::hal::paging2::{TTable, TTableTy};
	use crate::memory::physical::highmem;
	use super::*;

	#[test]
	fn unmapped_page_doesnt_translate() {
		let table = TTableTy::new(&*KERNEL_PAGE_TABLE.read(), highmem()).unwrap();
		assert_eq!(table.translate_page(Page::new(VirtualAddress::new(0xcafebabe000))), None);
		assert_eq!(table.translate_page(Page::new(VirtualAddress::new(0xdeadbeef000))), None);
		assert_eq!(table.translate_page(Page::new(VirtualAddress::new(0x347e40000))), None);
	}

	#[test]
	fn unmapped_address_doesnt_translate() {
		let table = TTableTy::new(&*KERNEL_PAGE_TABLE.read(), highmem()).unwrap();
		assert_eq!(table.translate_address(VirtualAddress::new(0xcafebabe)), None);
		assert_eq!(table.translate_address(VirtualAddress::new(0xdeadbeef)), None);
		assert_eq!(table.translate_address(VirtualAddress::new(0x347e40)), None);
	}

	#[test]
	fn translations_after_mapping() {
		let mut table = TTableTy::new(&*KERNEL_PAGE_TABLE.read(), highmem()).unwrap();
		table.map_page(
			Page::new(VirtualAddress::new(0xcafebabe000)),
			Frame::new(PhysicalAddress::new(0x347e40000)),
		).expect("Page not yet mapped");
		assert_eq!(
			table.translate_page(Page::new(VirtualAddress::new(0xcafebabe000))),
			Some(Frame::new(PhysicalAddress::new(0x347e40000)))
		);
	}

	#[test]
	fn cannot_overmap() {
		let mut table = TTableTy::new(&*KERNEL_PAGE_TABLE.read(), highmem()).unwrap();
		table.map_page(
			Page::new(VirtualAddress::new(0xcafebabe000)),
			Frame::new(PhysicalAddress::new(0x347e40000)),
		).expect("Page not yet mapped");
		table.map_page(
			Page::new(VirtualAddress::new(0xcafebabe000)),
			Frame::new(PhysicalAddress::new(0xcafebabe000)),
		).expect_err("Page already mapped");
	}

	#[test]
	fn address_offset() {
		let mut table = TTableTy::new(&*KERNEL_PAGE_TABLE.read(), highmem()).unwrap();
		table.map_page(
			Page::new(VirtualAddress::new(0xcafebabe000)),
			Frame::new(PhysicalAddress::new(0x347e40000)),
		).expect("Page not yet mapped");
		assert_eq!(
			table.translate_address(VirtualAddress::new(0xcafebabe123)),
			Some(PhysicalAddress::new(0x347e40123))
		)
	}
}
