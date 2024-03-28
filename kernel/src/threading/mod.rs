use alloc::borrow::Cow;
use core::arch::asm;
use core::cmp::Ordering;
use core::num::NonZeroUsize;
use core::time::Duration;
use log::{debug, warn};
use kernel_api::memory::mapping::Stack;
use kernel_api::memory::physical::{highmem, OwnedFrames};
use kernel_api::memory::r#virtual::{Global, OwnedPages};
use kernel_api::time::Instant;
use crate::hal::{Hal, HalTy, ThreadControlBlock, ThreadState};
use scheduler::Tid;
use crate::hal::timing::{Timer, Eoi};
use crate::interrupts::irq_handler;

pub mod scheduler;

pub unsafe fn init(handoff_data: crate::HandoffWrapper) -> Tid {
	let stack = handoff_data.memory.stack;
	let ttable = handoff_data.to_empty_ttable();

	// fixme: is highmem always correct?
	let stack_phys_len = stack.top_virt - stack.bottom_virt - 1;
	let stack_frames = OwnedFrames::from_raw_parts(
		stack.top_phys - stack_phys_len,
		NonZeroUsize::new(stack_phys_len).expect("Cannot have a zero sized stack"),
		highmem()
	);
	let stack_pages = OwnedPages::from_raw_parts(
		stack.bottom_virt,
		NonZeroUsize::new(stack_phys_len + 1).expect("Cannot have a zero sized stack"),
		Global
	);

	let mut scheduler = scheduler::SCHEDULER.lock();
	let tcb =  ThreadControlBlock {
		name: Cow::Borrowed("init"),
		kernel_stack: Stack::from_raw_parts(stack_frames, stack_pages),
		ttable,
		state: ThreadState::Running,
		save_state: Default::default(),
	};
	
	scheduler.init(tcb);

	Tid(0)
}

pub fn thread_yield() {
	defer_schedule();
}

pub fn block(reason: ThreadState) {
	scheduler::SCHEDULER.lock().block(reason);
}

#[derive(Debug, Copy, Clone)]
struct SchedulerEvent {
	time: Instant,
	tid: Tid,
	action: EventTy,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum EventTy {
	Unblock,
}

impl Ord for SchedulerEvent {
	fn cmp(&self, other: &Self) -> Ordering {
		self.time.cmp(&other.time)
	}
}

impl PartialOrd for SchedulerEvent {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl PartialEq for SchedulerEvent {
	fn eq(&self, other: &Self) -> bool {
		self.time.eq(&other.time)
	}
}

impl Eq for SchedulerEvent {}

fn push_to_global_sleep_queue(wake_time: Instant) {
	todo!()
}

fn pinned_sleep(time_of_wake: Instant) {
	let mut guard = scheduler::SCHEDULER.lock();
	let sleep_event = SchedulerEvent {
		tid: guard.current_tid(),
		time: time_of_wake,
		action: EventTy::Unblock
	};
	debug!("sleeping tid {:?}", sleep_event.tid);
	guard.event_queue.add(sleep_event);
	guard.block(ThreadState::Blocked);
}

pub fn sleep(duration: Duration) {
	if duration <= Duration::from_secs(1) {
		// Core pinned sleep
		pinned_sleep(Instant::now() + duration);
	} else {
		todo!();
		push_to_global_sleep_queue(Instant::now() + duration);
	}
}

pub fn sleep_until(wake_time: Instant) {
	// to avoid having to calculate time until wake, always do a pinned sleep and have it pulled from back of queue later
	pinned_sleep(wake_time);
}

#[naked]
pub unsafe extern "C" fn thread_startup() {
	extern "C" fn thread_startup_inner() {
		unsafe {
			scheduler::SCHEDULER.unlock();
		}
	}

	asm!(
	"pop rbp", // aligns to 16 bytes
	"call {}",
	"pop rdi", // pop args off stack
	"pop rsi",
	"pop rdx",
	"pop rcx",
	"ret",
	sym thread_startup_inner, options(noreturn));
}

pub fn defer_schedule() {
	crate::hal::arch::apic::send_self_ipi(0x30);
}
