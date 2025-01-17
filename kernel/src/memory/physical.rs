use core::mem;
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel_api::memory::allocator::BackingAllocator;
use kernel_api::sync::{RwLock, RwUpgradableReadGuard, RwWriteGuard};
use kernel_api::memory::physical::GlobalAllocator;

#[export_name = "__popcorn_memory_physical_highmem"]
static GLOBAL_HIGHMEM: GlobalAllocator = GlobalAllocator { rwlock: RwLock::new(None) };
#[export_name = "__popcorn_memory_physical_dmamem"]
static GLOBAL_DMA: GlobalAllocator = GlobalAllocator { rwlock: RwLock::new(None) };

pub use kernel_api::memory::physical::{highmem, dmamem};

pub fn init_highmem<'a>(allocator: &'static dyn BackingAllocator) {
	GLOBAL_HIGHMEM.rwlock.write().replace(allocator);
}

pub fn init_dmamem<'a>(allocator: &'static dyn BackingAllocator) {
	GLOBAL_DMA.rwlock.write().replace(allocator);
}

pub fn with_highmem_as<'a, R>(allocator: &'a dyn BackingAllocator, f: impl FnOnce() -> R) -> R {
	// FIXME: huge issue in that all allocations get lost therefore only safe to use for bootstrap
	// FIXME(soundness): is this sound?

	let mut write_lock = GLOBAL_HIGHMEM.rwlock.write();
	let static_highmem = unsafe { mem::transmute::<_, &'static _>(allocator) };
	let old_highmem = write_lock.replace(static_highmem);

	// To prevent the allocator being changed while the closure is executing, downgrade the write lock to a read lock held across the boundary
	let read_lock = RwWriteGuard::downgrade_to_upgradable(write_lock);

	struct DropGuard<'a, T> {
		lock: ManuallyDrop<RwUpgradableReadGuard<'a, T>>,
		old_val: ManuallyDrop<T>
	}
	impl<T> Drop for DropGuard<'_, T> {
		fn drop(&mut self) {
			let lock = unsafe { ManuallyDrop::take(&mut self.lock) };
			let old_val = unsafe { ManuallyDrop::take(&mut self.old_val) };

			let mut lock = RwUpgradableReadGuard::upgrade(lock);
			*lock = old_val;
		}
	}

	let _drop_guard = DropGuard {
		lock: ManuallyDrop::new(read_lock),
		old_val: ManuallyDrop::new(old_highmem)
	};

	f()
}

static REFCOUNTS: [RefCountEntry; 0] = [];

struct RefCountEntry {
	strong_count: AtomicUsize,
	next_segment: Option<AtomicUsize>
}

impl RefCountEntry {
	fn increment(&self) {
		self.strong_count.fetch_add(1, Ordering::Relaxed);
	}

	fn decrement(&self) -> bool {
		self.strong_count.fetch_sub(1, Ordering::Relaxed) == 1
	}
}
