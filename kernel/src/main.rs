#![feature(custom_test_frameworks)]
#![test_runner(tests::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]
#![feature(pointer_byte_offsets)]
#![feature(lang_items)]
#![feature(allocator_api)]
#![no_std]
#![no_main]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::{Cell, RefCell};
use crate::io::serial;
use crate::io::serial::SERIAL0;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr::slice_from_raw_parts_mut;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel_exports::sync::Lock;

mod sync;
mod io;

#[export_name = "_start"]
extern "sysv64" fn kstart(handoff_data: utils::handoff::Data) -> ! {
	serial::init_serial0().expect("Failed to initialise serial0");
	writeln!(SERIAL0.lock(), "Hello world!").unwrap();

	//#[cfg(test)] test_main();
	#[cfg(not(test))] kmain(handoff_data)
}

fn kmain(mut handoff_data: utils::handoff::Data) -> ! {
	writeln!(SERIAL0.lock(), "Handoff data:\n{handoff_data:x?}").unwrap();

	/*let mut wmark = WatermarkAllocator::new(&mut handoff_data.memory.map);

	unsafe {
		// SAFETY: unset a few lines below
		memory::alloc::phys::GLOBAL_ALLOCATOR.set_unchecked(&mut wmark);
	}
	let thingy = (handoff_data.modules.phys_allocator_start)(Range(Frame::new(PhysicalAddress(0)), Frame::new(PhysicalAddress(0x10000))));
	memory::alloc::phys::GLOBAL_ALLOCATOR.unset();*/

	if let Some(fb) = handoff_data.framebuffer {
		let size = fb.stride * fb.height;
		for pixel in unsafe { &mut *slice_from_raw_parts_mut(fb.buffer.cast::<u32>(), size) }.iter_mut() {
			*pixel = 0xeeeeee;
		}
	}

	loop {}
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
	let _ = writeln!(SERIAL0.lock(), "{info}");
	loop {

	}
}

#[no_mangle]
pub fn __popcorn_module_panic(info: &PanicInfo) -> ! {
	let _ = writeln!(SERIAL0.lock(), "Panic from module: {info}");
	loop {

	}
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_alloc(layout: Layout) -> *mut u8 {
	alloc::alloc::alloc(layout)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_dealloc(ptr: *mut u8, layout: Layout) {
	alloc::alloc::dealloc(ptr, layout)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_alloc_zeroed(layout: Layout) -> *mut u8 {
	alloc::alloc::alloc_zeroed(layout)
}

#[no_mangle]
pub unsafe extern "Rust" fn __popcorn_module_realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
	alloc::alloc::realloc(ptr, layout, new_size)
}

mod arch {
	use core::arch::asm;
	use core::marker::PhantomData;

	pub trait PortWidth {}
	impl PortWidth for u8 {}
	impl PortWidth for u16 {}
	impl PortWidth for u32 {}

	#[derive(Debug, Copy, Clone)]
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

#[global_allocator]
static Allocator: Foo = Foo;

struct Foo;
unsafe impl GlobalAlloc for Foo {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		todo!()
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		todo!()
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		todo!()
	}
}
