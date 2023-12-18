// rust features
#![feature(custom_test_frameworks)]
#![test_runner(test_harness::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(const_trait_impl)]
#![feature(allocator_api)]
#![feature(let_chains)]
#![feature(specialization)]
#![feature(const_type_name)]
#![feature(inline_const)]
#![feature(decl_macro)]
#![feature(abi_x86_interrupt)]
#![feature(generic_arg_infer)]
#![feature(panic_info_message)]
#![feature(gen_blocks)]
#![feature(maybe_uninit_uninit_array)]
#![feature(type_changing_struct_update)]
#![feature(maybe_uninit_array_assume_init)]
#![feature(dyn_star)]
#![feature(inherent_associated_types)]
#![feature(generic_const_exprs)]
#![feature(pointer_like_trait)]
#![feature(exclusive_range_pattern)]
#![feature(int_roundings)]

#![feature(kernel_heap)]
#![feature(kernel_allocation_new)]

#![no_std]
#![no_main]

#![deny(deprecated)]

extern crate alloc;
#[cfg(panic = "unwind")]
extern crate unwinding;

extern crate self as kernel;

use alloc::sync::Arc;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::cmp::max;
use crate::io::serial;
use core::fmt::Write;
use core::mem;
use core::num::NonZeroUsize;
use core::ops::Deref;
use core::panic::PanicInfo;
use core::ptr::{addr_of_mut, slice_from_raw_parts_mut};
use log::{debug, error, info, trace};
use hal_amd64::idt::entry::{Entry, Type};
use kernel_api::memory::PhysicalAddress;
use kernel_api::sync::Mutex;
use kernel_api::memory::{allocator::BackingAllocator};

mod sync;
mod io;
mod memory;
mod panicking;
mod resource;
mod logging;

#[cfg(test)]
pub mod test_harness;

#[macro_export]
macro_rules! usize {
    ($stuff:expr) => {usize::try_from($stuff).unwrap()};
}

#[macro_export]
macro_rules! u64 {
    ($stuff:expr) => {u64::try_from($stuff).unwrap()};
}

#[macro_export]
macro_rules! into {
    ($stuff:expr) => {($stuff).try_into().unwrap()};
}

#[export_name = "_start"]
extern "sysv64" fn kstart(handoff_data: &utils::handoff::Data) -> ! {
	let result = serial::init_serial0();
	result.expect("Failed to initialise serial0");
	sprintln!("Hello world!");

	#[cfg(not(test))] kmain(handoff_data);
	#[cfg(test)] {
		test_main();
		unreachable!("test harness returned")
	}
}

use hal_amd64::idt::handler::{InterruptStackFrame, PageFaultError};
use hal_amd64::idt::Idt;
use kernel_api::memory::{Frame};
use kernel_api::memory::allocator::Config;
use utils::handoff::MemoryType;
use crate::resource::watermark_allocator::WatermarkAllocator;

fn kmain(mut handoff_data: &utils::handoff::Data) -> ! {
	let _ = logging::init();

	let map = unsafe { handoff_data.log.symbol_map.map(|ptr| ptr.as_ref()) };
	*panicking::SYMBOL_MAP.write() = map;

	trace!("Handoff data:\n{handoff_data:x?}");

	hal_amd64::__popcorn_hal_early_init();

	unsafe { asm!("int3"); }

	let usable_memory = handoff_data.memory.map.iter().filter(|entry|
		entry.ty == MemoryType::Free
			|| entry.ty == MemoryType::AcpiReclaim
			|| entry.ty == MemoryType::BootloaderCode
			|| entry.ty == MemoryType::BootloaderData
	);

	// Split allocator system is used when a significant portion of memory is above the 4GiB boundary
	// This allows better optimization for non-DMA allocations as well as reducing pressure on memory usable by DMA
	// The current algorithm uses split allocators when the total amount of non-DMA memory is >= 1GiB
	let split_allocators = if cfg!(not(target_pointer_width = "32")) {
		const FOUR_GB: PhysicalAddress = PhysicalAddress::new(1<<32);

		let bytes_over_4gb: usize = usable_memory.clone()
				.filter(|entry| entry.start() >= FOUR_GB)
				.map(|entry| entry.end() - entry.start())
				.sum();

		bytes_over_4gb >= 1024*1024*1024
	} else { false };

	info!("Split allocator: {}", if split_allocators { "enabled" } else { "disabled" });

	panicking::stack_trace();

	{
		use kernel_api::memory::PhysicalAddress;

		if split_allocators {
			todo!("split allocators not supported yet :(");
		}

		let max_usable_memory = usable_memory.clone()
			.max_by(|a, b| a.end().cmp(&b.end()))
			.expect("Free memory should exist");
		let max_usable_memory = max_usable_memory.end();

		let mut spaces = usable_memory.clone()
			.map(|entry| {
				Frame::new(entry.start().align_up())..Frame::new(entry.end().align_down())
			});

		let mut spaces2 = spaces.clone();
		let mut watermark_allocator = WatermarkAllocator::new(&mut spaces2);
		let mut new_alloc = memory::physical::with_highmem_as(&watermark_allocator, || {
			// TODO: initialise heap
			<bitmap_allocator::Wrapped as BackingAllocator>::new(
				Config {
					allocation_range: Frame::new(PhysicalAddress::new(0))..Frame::new(max_usable_memory.align_down()),
					regions: &mut spaces
				},
				[]
			)
		});

		let allocator = Arc::get_mut(&mut new_alloc).expect("No other references to allocator should exist yet");
		watermark_allocator.drain_into(allocator);

		let allocator = memory::physical::highmem();
	}

/*
	{
		let wmark = WatermarkAllocator::new(&handoff_data.memory.map);
		// SAFETY: We `take()` the allocator at the end of this block, so the allocator is "static" for the time in between
		let static_wmark = unsafe { mem::transmute::<&dyn Allocator, &'static dyn Allocator>(&wmark) };
		memory::alloc::phys::GLOBAL_HIGH_MEM_ALLOCATOR.set(static_wmark);

		sprintln!("{:#x?}", memory::paging3::CURRENT_PAGE_TABLE.lock().unwrap().debug_mappings());
		let mut baz = memory::paging3::InactiveTable::new().unwrap();
		memory::paging3::CURRENT_PAGE_TABLE.lock().unwrap().modify_with(&mut baz, |m| {
			sprintln!("{:#x?}", m.debug_mappings());
			sprintln!("{:#x?}", m.debug_table());
		});
		sprintln!("{:#x?}", memory::paging3::CURRENT_PAGE_TABLE.lock().unwrap().debug_mappings());

		memory::alloc::phys::GLOBAL_HIGH_MEM_ALLOCATOR.take();
	}

	let split_allocators = false; // todo
	/*unsafe {
		// SAFETY: unset a few lines below
		memory::alloc::phys::GLOBAL_ALLOCATOR.set_unchecked(&mut wmark);
	}
	let thingy = (handoff_data.modules.phys_allocator_start)(Range(Frame::new(PhysicalAddress(0)), Frame::new(PhysicalAddress(0x10000))));
	memory::alloc::phys::GLOBAL_ALLOCATOR.unset();*/
*/
	if let Some(ref fb) = handoff_data.framebuffer {
		let size = fb.stride * fb.height;
		for pixel in unsafe { &mut *slice_from_raw_parts_mut(fb.buffer.cast::<u32>(), size) } {
			*pixel = 0xeeeeee;
		}
	}

	loop {}
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
	sprint!("\u{001b}[31m\u{001b}[1mPANIC:");
	if let Some(location) = info.location() {
		sprint!(" {location}");
	}
	sprintln!("\u{001b}[0m");

	if let Some(message) = info.message() {
		sprintln!("{}", *message);
	} else if let Some(payload) = info.payload().downcast_ref::<&'static str>() {
		sprintln!("{}", payload);
	}

	panicking::do_panic()
}

#[no_mangle]
pub extern "Rust" fn __popcorn_module_panic(info: &PanicInfo) -> ! {
	panic!("Panic from module: {info}");
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_alloc(layout: Layout) -> *mut u8 {
	alloc::alloc::alloc(layout)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_dealloc(ptr: *mut u8, layout: Layout) {
	alloc::alloc::dealloc(ptr, layout);
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_alloc_zeroed(layout: Layout) -> *mut u8 {
	alloc::alloc::alloc_zeroed(layout)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
	alloc::alloc::realloc(ptr, layout, new_size)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_is_panicking() -> bool { panicking::panicking() }

mod arch {
	use core::arch::asm;
	use core::marker::PhantomData;

	pub trait PortWidth {}
	impl PortWidth for u8 {}
	impl PortWidth for u16 {}
	impl PortWidth for u32 {}

	#[derive(Debug)]
	pub struct Port<T>(u16, PhantomData<T>) where T: PortWidth;

	impl<T> Port<T> where T: PortWidth {
		pub const fn new(addr: u16) -> Self {
			Self(addr, PhantomData)
		}
	}

	impl Port<u8> {
		#[inline(always)]
		pub unsafe fn read(&self) -> u8 {
			let ret;
			unsafe { asm!("in al, dx", in("dx") self.0, out("al") ret, options(nostack, preserves_flags)); }
			ret
		}

		#[inline(always)]
		pub unsafe fn write(&mut self, val: u8) {
			unsafe { asm!("out dx, al", in("dx") self.0, in("al") val, options(nostack, preserves_flags)); }
		}
	}

	impl Port<u16> {
		#[inline(always)]
		pub unsafe fn read(&self) -> u16 {
			let ret;
			unsafe { asm!("in ax, dx", in("dx") self.0, out("ax") ret, options(nostack, preserves_flags)); }
			ret
		}

		#[inline(always)]
		pub unsafe fn write(&mut self, val: u16) {
			unsafe { asm!("out dx, ax", in("dx") self.0, in("ax") val, options(nostack, preserves_flags)); }
		}
	}

	impl Port<u32> {
		#[inline(always)]
		pub unsafe fn read(&self) -> u32 {
			let ret;
			unsafe { asm!("in eax, dx", in("dx") self.0, out("eax") ret, options(nostack, preserves_flags)); }
			ret
		}

		#[inline(always)]
		pub unsafe fn write(&mut self, val: u32) {
			unsafe { asm!("out dx, eax", in("dx") self.0, in("eax") val, options(nostack, preserves_flags)); }
		}
	}
}

mod allocator {
	use core::alloc::{GlobalAlloc, Layout};
	use core::ptr;
	use core::ptr::NonNull;
	use log::{debug, trace};
	use kernel_api::memory::heap::{AllocError, Heap};

	struct HookAllocator;

	unsafe impl GlobalAlloc for HookAllocator {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			debug!("alloc({layout:?})");
			match kernel_default_heap::__popcorn_kernel_heap_allocate(layout) {
				Ok(ptr) => ptr.as_ptr(),
				Err(_) => ptr::null_mut()
			}
		}

		unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
			match NonNull::new(ptr) {
				Some(ptr) => kernel_default_heap::__popcorn_kernel_heap_deallocate(ptr, layout),
				None => {}
			}
		}
	}

	#[cfg_attr(not(test), global_allocator)]
	static ALLOCATOR: HookAllocator = HookAllocator;
}

#[cfg(test)]
mod tests {
	#[test]
	fn trivial_assertion() {
		assert_eq!(1, 1);
	}

	#[test]
	#[should_panic]
	fn foobar() {
		assert_eq!(1, 3);
	}


	#[test]
	#[should_panic]
	fn no_panic() {
		assert_ne!(1, 3);
	}

	#[test]
	#[should_panic = "help"]
	fn help_panic() { panic!("help"); }

	#[test]
	#[should_panic = "help"]
	fn bob_panic() { panic!("bob"); }

	#[test]
	#[should_panic = "help\n"]
	fn help_panicnl() { panic!("help"); }

	#[test]
	fn foobar_fail() {
		assert_eq!(1, 3);
	}

	#[test]
	#[ignore = "ignoring this for now"]
	fn foobar_ignored() {
		assert_eq!(1, 3);
	}
}
