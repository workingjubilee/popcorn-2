#![unstable(feature = "kernel_mmap", issue = "24")]

use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::num::NonZeroUsize;
use log::debug;
use crate::memory::allocator::{AlignedAllocError, AllocationMeta, BackingAllocator, ZeroAllocError};
use crate::memory::{AllocError, Frame, Page};
use crate::memory::physical::{OwnedFrames, highmem};
use crate::memory::r#virtual::{Global, OwnedPages, VirtualAllocator};

#[derive(Copy, Clone, Debug)]
pub struct Highmem;

unsafe impl BackingAllocator for Highmem {
	fn allocate_contiguous(&self, frame_count: usize) -> Result<Frame, crate::memory::allocator::AllocError> {
		highmem().allocate_contiguous(frame_count)
	}

	fn allocate_one(&self) -> Result<Frame, crate::memory::allocator::AllocError> {
		highmem().allocate_one()
	}

	fn try_allocate_zeroed(&self, frame_count: usize) -> Result<Frame, ZeroAllocError> {
		highmem().try_allocate_zeroed(frame_count)
	}

	fn allocate_zeroed(&self, frame_count: usize) -> Result<Frame, crate::memory::allocator::AllocError> {
		highmem().allocate_zeroed(frame_count)
	}

	unsafe fn deallocate_contiguous(&self, base: Frame, frame_count: NonZeroUsize) {
		highmem().deallocate_contiguous(base, frame_count)
	}

	fn push(&mut self, allocation: AllocationMeta) {
		unimplemented!()
	}

	fn allocate_contiguous_aligned(&self, count: NonZeroUsize, alignment_log2: u32) -> Result<Frame, crate::memory::allocator::AllocError> {
		highmem().allocate_contiguous_aligned(count, alignment_log2)
	}

	fn try_allocate_contiguous_aligned(&self, count: NonZeroUsize, alignment_log2: u32) -> Result<Frame, AlignedAllocError> {
		highmem().try_allocate_contiguous_aligned(count, alignment_log2)
	}
}

/// An owned region of memory
///
/// Depending on memory attributes, this may be invalid to read or write to
#[derive(Debug)]
pub struct Mapping<A: BackingAllocator = Highmem> {
	base: Page,
	len: usize,
	allocator: A
}

impl<A: BackingAllocator> Mapping<A> {
	pub fn as_ptr(&self) -> *const u8 {
		self.base.as_ptr()
	}
	
	pub fn as_mut_ptr(&mut self) -> *mut u8 {
		self.base.as_ptr()
	}
	
	pub fn new_with(len: usize, physical_allocator: A) -> Result<Self, AllocError> {
		// FIXME: memory leak here on error from lack of ArcFrame
		let physical_mem = physical_allocator.allocate_contiguous(len)?;
		let virtual_mem = Global.allocate_contiguous(len)?;

		// TODO: huge pages
		let mut page_table = unsafe { crate::bridge::paging::__popcorn_paging_get_current_page_table() };
		for (frame, page) in (0..len).map(|i| (physical_mem + i, virtual_mem + i)) {
			unsafe { crate::bridge::paging::__popcorn_paging_map_page(&mut page_table, page, frame, &physical_allocator) }
					.expect("todo");
		}

		Ok(Self {
			base: virtual_mem,
			len,
			allocator: physical_allocator
		})
	}
}

impl Mapping<Highmem> {
	pub fn new(len: usize) -> Result<Self, AllocError> {
		Self::new_with(len, Highmem)
	}

	fn resize_inner(&mut self, new_len: usize) -> Result<(), Option<Frame>> {
		if new_len == self.len { return Ok(()); }

		// FIXME: DOnT JUST USE HIGHMEM UnCOnDITIOnALLY
		let original_physical_allocator = self.allocator;

		if new_len < self.len {
			// todo: actually free and unmap the extra memory

			self.len = new_len;
			Ok(())
		} else {
			let extra_len = new_len - self.len;

			debug!("allocating extra physical memory");
			// fixme: physical mem leak
			let extra_physical_mem = original_physical_allocator.allocate_contiguous(extra_len).map_err(|_| None)?;
			debug!("allocating extra virtual memory");
			let extra_virtual_mem = Global.allocate_contiguous_at(self.base + self.len, extra_len);

			match extra_virtual_mem {
				Ok(_) => {
					let start_of_extra = self.base + self.len;

					// TODO: huge pages
					let mut page_table = unsafe { crate::bridge::paging::__popcorn_paging_get_current_page_table() };

					for (frame, page) in (0..extra_len).map(|i| (extra_physical_mem + i, start_of_extra + i)) {
						unsafe { crate::bridge::paging::__popcorn_paging_map_page(&mut page_table, page, frame, &original_physical_allocator) }
								.expect("todo");
					}

					self.len = new_len;
					Ok(())
				}
				Err(_) => Err(Some(extra_physical_mem))
			}
		}
	}

	pub fn resize_in_place(&mut self, new_len: usize) -> Result<(), AllocError> {
		self.resize_inner(new_len)
				.map_err(|_| AllocError)
	}

	pub fn resize(&mut self, new_len: usize) -> Result<(), AllocError> {
		match self.resize_inner(new_len) {
			Ok(_) => Ok(()),
			Err(None) => Err(AllocError),
			Err(Some(extra_physical_mem)) => {
				// can assume here that new_len > len as shrinking can't fail

				// FIXME: DOnT JUST USE HIGHMEM UnCOnDITIOnALLY
				let original_physical_allocator = self.allocator;

				let extra_len = new_len - self.len;
				let new_virtual_mem = Global.allocate_contiguous(new_len)?;

				let mut page_table = unsafe { crate::bridge::paging::__popcorn_paging_get_current_page_table() };

				let physical_base: Frame = todo!();
				for (frame, page) in (0..self.len).map(|i| (physical_base + i, new_virtual_mem + i)) {
					unsafe { crate::bridge::paging::__popcorn_paging_map_page(&mut page_table, page, frame, &original_physical_allocator) }.expect("todo");
				}
				for (frame, page) in (0..extra_len).map(|i| (extra_physical_mem + i, new_virtual_mem + self.len + i)) {
					unsafe { crate::bridge::paging::__popcorn_paging_map_page(&mut page_table, page, frame, &original_physical_allocator) }.expect("todo");
				}

				self.base = new_virtual_mem;
				self.len = new_len;

				Ok(())
			}
		}
	}

	/*pub fn remap(&mut self, new_len: usize) -> Result<(), AllocError> {
		if new_len == self.len { return Ok(()); }

		let original_physical_allocator: &dyn BackingAllocator = todo!("retrieve original allocator");
		let physical_base: Frame = todo!("translate base to locate physical backing");

		if new_len < self.len {
			// todo: actually free and unmap the extra memory

			self.len = new_len;
			Ok(())
		} else {
			let extra_len = new_len - self.len;

			let extra_physical_mem = original_physical_allocator.allocate_contiguous(extra_len)?;
			let extra_virtual_mem = Global.allocate_contiguous_at(self.base + self.len, extra_len);

			match extra_virtual_mem {
				Ok(_) => {
					let start_of_extra = self.base + self.len;

					// TODO: huge pages
					let mut page_table = current_page_table();

					for (frame, page) in (0..extra_len).map(|i| (extra_physical_mem + i, start_of_extra + i)) {
						page_table.map_page(page, frame, original_physical_allocator).expect("todo");
					}

					self.len = new_len;
					Ok(())
				}
				Err(_) => {
					let new_virtual_mem = Global.allocate_contiguous(new_len)?;

					let mut page_table = current_page_table();

					for (frame, page) in (0..self.len).map(|i| (physical_base + i, new_virtual_mem + i)) {
						page_table.map_page(page, frame, original_physical_allocator).expect("todo");
					}
					for (frame, page) in (0..extra_len).map(|i| (extra_physical_mem + i, new_virtual_mem + self.len + i)) {
						page_table.map_page(page, frame, original_physical_allocator).expect("todo");
					}

					self.base = new_virtual_mem;
					self.len = new_len;

					Ok(())
				}
			}
		}
	}*/

	pub fn into_raw_parts(self) -> (Page, usize) {
		let Self { base, len, .. } = self;
		(base, len)
	}

	pub unsafe fn from_raw_parts(base: Page, len: usize) -> Self {
		Self {
			base,
			len,
			allocator: Highmem
		}
	}

	pub fn end(&self) -> Page {
		self.base + self.len
	}

	pub fn len(&self) -> usize {
		self.len
	}
}

impl<A: BackingAllocator> Drop for Mapping<A> {
	fn drop(&mut self) {
		// todo
	}
}

pub trait RawMap {
	fn physical_length_to_virtual_length(physical_length: NonZeroUsize) -> NonZeroUsize;
	fn physical_start_offset_from_virtual() -> isize;
}

pub struct NewMap<'phys_allocator, R: RawMap, A: VirtualAllocator> {
	raw: PhantomData<R>,
	physical: OwnedFrames<'phys_allocator>,
	virtual_base: Page,
	virtual_allocator: ManuallyDrop<A>,
}

// todo: replace this mess with builder
impl<R: RawMap> NewMap<'static, R, Global> {
	pub fn new(len: NonZeroUsize) -> Result<Self, AllocError> {
		Self::new_with(len, highmem(), Global)
	}
}

impl<'phys_alloc, R: RawMap, A: VirtualAllocator> NewMap<'phys_alloc, R, A> {
	pub fn new_with(len: NonZeroUsize, physical_allocator: &'phys_alloc dyn BackingAllocator, virtual_allocator: A) -> Result<Self, AllocError> {
		let virtual_len = R::physical_length_to_virtual_length(len);
		let physical_len = len;

		let physical_mem = OwnedFrames::new_with(physical_len, physical_allocator)?;
		let virtual_mem = OwnedPages::new_with(virtual_len, virtual_allocator)?;

		let physical_base = physical_mem.base;
		let (virtual_base, _, virtual_allocator) = virtual_mem.into_raw_parts();
		let offset_base = virtual_base + R::physical_start_offset_from_virtual();

		// TODO: huge pages
		let mut page_table = unsafe { crate::bridge::paging::__popcorn_paging_get_current_page_table() };
		for (frame, page) in (0..physical_len.get()).map(|i| (physical_base + i, offset_base + i)) {
			unsafe { crate::bridge::paging::__popcorn_paging_map_page(&mut page_table, page, frame, &physical_allocator) }
					.expect("Virtual memory uniquely owned by the allocation so should not be mapped in this address space");
		}

		Ok(Self {
			raw: PhantomData,
			physical: physical_mem,
			virtual_base,
			virtual_allocator: ManuallyDrop::new(virtual_allocator)
		})
	}
}

impl<R: RawMap, A: VirtualAllocator> Drop for NewMap<'_, R, A> {
	fn drop(&mut self) {
		// todo: unmap stuff

		let virtual_allocator = unsafe { ManuallyDrop::take(&mut self.virtual_allocator) };
		let _pages = unsafe {
			OwnedPages::from_raw_parts(
				self.virtual_base,
				R::physical_length_to_virtual_length(self.physical.len),
				virtual_allocator
			)
		};
	}
}

pub enum MmapRawMap {}

impl RawMap for MmapRawMap {
	fn physical_length_to_virtual_length(physical_length: NonZeroUsize) -> NonZeroUsize { physical_length }
	fn physical_start_offset_from_virtual() -> isize { 0 }
}

pub enum KstackRawMap {}

impl RawMap for KstackRawMap {
	fn physical_length_to_virtual_length(physical_length: NonZeroUsize) -> NonZeroUsize {
		physical_length.checked_add(1).expect("Stack size overflow")
	}
	fn physical_start_offset_from_virtual() -> isize { 1 }
}

pub type NewMmap<'phys_alloc, V: VirtualAllocator = Global> = NewMap<'phys_alloc, MmapRawMap, V>;
pub type Stack<'phys_alloc, V: VirtualAllocator = Global> = NewMap<'phys_alloc, KstackRawMap, V>;
