use alloc::collections::{BTreeMap, VecDeque};
use core::borrow::Borrow;
use core::cell::{Cell, UnsafeCell};
use core::cmp::min;
use core::fmt::{Debug, Formatter};
use core::marker::PhantomData;
use core::mem::{MaybeUninit, swap};
use core::num::NonZeroU128;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use crate::hal::{HalTy, Hal, ThreadControlBlock, ThreadState};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use log::{debug, warn};
use kernel_api::time::Instant;
use crate::hal::timing::{Timer, Eoi};
use crate::interrupts::irq_handler;
use crate::threading::{EventTy, SchedulerEvent};

#[thread_local]
pub static SCHEDULER: IrqCell<Scheduler> = IrqCell::new(Scheduler::new());

pub struct IrqCell<T: ?Sized> {
	state: Cell<Option<usize>>,
	data: UnsafeCell<T>
}

impl<T> IrqCell<T> {
	pub const fn new(val: T) -> Self {
		Self { state: Cell::new(None), data: UnsafeCell::new(val) }
	}
}

impl<T: ?Sized> IrqCell<T> {
	pub fn lock(&self) -> IrqGuard<'_, T> {
		// Unsafety: is this actually needed?
		if self.state.get().is_some() { panic!("IrqCell cannot be borrowed multiple times"); }

		self.state.set(Some(HalTy::get_and_disable_interrupts()));
		IrqGuard { cell: self, _phantom_not_send: PhantomData }
	}

	pub unsafe fn unlock(&self) {
		let old_state = self.state.take();
		HalTy::set_interrupts(old_state.unwrap());
	}
}

pub struct IrqGuard<'cell, T: ?Sized> {
	cell: &'cell IrqCell<T>,
	_phantom_not_send: PhantomData<*mut u8>, // Dropping guard on other core would cause interrupts to be enabled in the wrong place
}

impl<T: Debug> Debug for IrqGuard<'_, T> {
	fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
		f.debug_struct("IrqGuard")
				.field("cell", &**self)
				.finish()
	}
}

impl<T: ?Sized> Deref for IrqGuard<'_, T> {
	type Target = T;

	fn deref(&self) -> &T {
		unsafe { &*self.cell.data.get() }
	}
}

impl<T: ?Sized> DerefMut for IrqGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		unsafe { &mut *self.cell.data.get() }
	}
}

impl<T: ?Sized> Drop for IrqGuard<'_, T> {
	fn drop(&mut self) {
		unsafe { self.cell.unlock(); }
	}
}

#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct Tid(pub(super) usize);

impl Tid {
	fn new() -> Self {
		static TIDS: AtomicUsize = AtomicUsize::new(1);
		Self(TIDS.fetch_add(1, Ordering::Relaxed))
	}
}

#[derive(Debug)]
pub struct Scheduler {
	tasks: BTreeMap<Tid, ThreadControlBlock>,
	run_queue: VecDeque<Tid>,
	current_tid: Tid,
	pub event_queue: EventQueue,
}

#[derive(Debug)]
pub struct EventQueue {
	events: VecDeque<SchedulerEvent>,
	yield_event: Option<(Instant, Tid)>,
}

impl EventQueue {
	pub fn add(&mut self, event: SchedulerEvent) {
		self.events.push_back(event);
		self.events.make_contiguous().sort_unstable();
	}
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DuplicateKey;

trait BTreeExt<K, V> {
	fn get_many_mut<const N: usize, Q>(&mut self, keys: [&Q; N]) -> Result<[Option<&mut V>; N], DuplicateKey> where K: Borrow<Q> + Ord, Q: Ord + ?Sized;
}

impl<K, V, A: core::alloc::Allocator + Clone> BTreeExt<K, V> for BTreeMap<K, V, A> {
	fn get_many_mut<const N: usize, Q>(&mut self, keys: [&Q; N]) -> Result<[Option<&mut V>; N], DuplicateKey> where K: Borrow<Q> + Ord, Q: Ord + ?Sized {
		fn get_ptr<Q, K, V, A: core::alloc::Allocator + Clone>(this: &mut BTreeMap<K, V, A>, key: &Q) -> Option<NonNull<V>> where K: Borrow<Q> + Ord, Q: Ord + ?Sized {
			this.get_mut(key).map(NonNull::from)
		}

		let mut ptrs = [MaybeUninit::<Option<NonNull<V>>>::uninit(); N];

		for (i, &cur) in keys.iter().enumerate() {
			let ptr = get_ptr(self, cur);

			if ptrs[..i].iter().any(|&prev| unsafe { *prev.assume_init_ref() } == ptr) {
				return Err(DuplicateKey);
			}

			ptrs[i].write(ptr);
		}

		Ok(unsafe { ptrs.transpose().assume_init() }.map(|ptr| ptr.map(|mut ptr| unsafe { ptr.as_mut() })))
	}
}

impl Scheduler {
	pub const fn new() -> Self {
		Self {
			tasks: BTreeMap::new(),
			run_queue: VecDeque::new(),
			current_tid: Tid(0),
			event_queue: EventQueue {
				events: VecDeque::new(),
				yield_event: None,
			}
		}
	}

	fn wake_and_reset_timer(&mut self) {
		debug!("event queue: {:#?}", self.event_queue);

		let mut local_timer = <HalTy as Hal>::LocalTimer::get();
		let tick_period = local_timer.get_time_period_picos().unwrap() * 4;

		let now = Instant::now();

		let ticks_to_event = |time: Instant| {
			let time = time.saturating_duration_since(now);
			let ticks = 1000 *  time.as_nanos() / u128::from(tick_period);
			NonZeroU128::new(ticks)
		};

		let mut timer_ticks = None::<NonZeroU128>;

		loop {
			let Some(event) = self.event_queue.events.get(0) else { break; };
			match ticks_to_event(event.time) {
				None => {
					let event = self.event_queue.events.pop_front().expect("Already peeked at this event");
					self.handle_event(event);
				},
				Some(event_ticks) => {
					timer_ticks = Some(timer_ticks.map(|current| min(current, event_ticks)).unwrap_or(event_ticks));
					break;
				},
			}
		}

		if let Some(yield_event) = self.event_queue.yield_event {
			match ticks_to_event(yield_event.0) {
				None => {
					self.event_queue.yield_event.take().expect("Already peeked at this event");
					super::defer_schedule();
				},
				Some(event_ticks) => {
					timer_ticks = Some(timer_ticks.map(|current| min(current, event_ticks)).unwrap_or(event_ticks));
				},
			}
		}

		if let Some(ticks) = timer_ticks {
			debug!("setting oneshot timer for {ticks} ticks");
			local_timer.set_oneshot_time(ticks.get()).unwrap();
		}
	}

	pub fn init(&mut self, tid0: ThreadControlBlock) {
		assert!(self.tasks.insert(Tid(0), tid0).is_none(), "Cannot init scheduler multiple times");

		let mut local_timer = <HalTy as Hal>::LocalTimer::get();
		local_timer.set_irq_number(0x40).unwrap();
		local_timer.set_divisor(4).unwrap();

		let eoi_handle = local_timer.eoi_handle();

		let timer_irq = irq_handler!(move || {
			main => {
				let mut guard = SCHEDULER.lock();
				guard.wake_and_reset_timer();
			}
			eoi => {
				eoi_handle.send();
			}
		});

		let scheduler_defer_irq = move || {
			let mut guard = SCHEDULER.lock();
			// FIXME: only true with LAPIC
			eoi_handle.send(); // safe to send this now since interrupts are disabled by scheduler lock
			guard.schedule();
		};

		assert!(crate::interrupts::insert_handler(0x40, timer_irq).is_none());
		crate::interrupts::set_defer_irq(scheduler_defer_irq);
	}

	pub fn current_tid(&self) -> Tid { self.current_tid }

	pub fn add_task(&mut self, tcb: ThreadControlBlock) -> Tid {
		let tid = Tid::new();
		self.tasks.insert(tid, tcb);
		self.run_queue.push_back(tid);
		super::defer_schedule();
		tid
	}

	pub fn schedule(&mut self) {
		let quantum = |_tid| {
			Duration::from_millis(50) // TODO: make this dynamic based on priority?
		};

		debug!("task schedule");
		if let Some(new_tid) = self.run_queue.pop_front() {
			let old_tid = self.current_tid;
			self.current_tid = new_tid;

			self.event_queue.yield_event = Some((
				Instant::now() + quantum(new_tid),
				new_tid
			));
			self.wake_and_reset_timer();

			let [old_tcb, new_tcb] = self.tasks.get_many_mut([&old_tid, &new_tid]).expect("Can't switch to same task");
			let old_tcb = old_tcb.expect("Cannot have been running a task that doesn't exist");
			let new_tcb = new_tcb.expect("Next task in queue has already exited");

			if old_tcb.state == ThreadState::Running {
				old_tcb.state = ThreadState::Ready;
				self.run_queue.push_back(old_tid);
			}

			new_tcb.state = ThreadState::Running;

			unsafe {
				HalTy::switch_thread(old_tcb, new_tcb);
			}
		} else {
			debug!("no other tasks to run");

			let current_tid = self.current_tid;
			let current_tcb = self.tasks.get(&current_tid).expect("Cannot have been running a task that doesn't exist");

			if current_tcb.state == ThreadState::Running {
				// No other tasks can get added to the run queue without an interrupt occuring so don't need to manually preempt
				self.event_queue.yield_event.take();
				self.wake_and_reset_timer();

				return;
			}

			todo!("idling");
		}
	}

	pub fn block(&mut self, state: ThreadState) {
		let current_tcb = self.tasks.get_mut(&self.current_tid).expect("Cannot have been running a task that doesn't exist");
		current_tcb.state = state;

		debug!("blocking {:?}", self.current_tid);

		// Remove the yield event for this task to not spuriously cut short a different task
		self.event_queue.yield_event.take();
		debug!("timer reset - block");
		self.wake_and_reset_timer();

		super::defer_schedule();
	}

	pub fn unblock(&mut self, tid: Tid) {
		debug!("unblocking {:?}", tid);
		if let Some(tcb) = self.tasks.get_mut(&tid) {
			tcb.state = ThreadState::Ready;
			self.run_queue.push_back(tid);
			super::defer_schedule();
		} else { warn!("Attempted to unblock dead {tid:?}"); }
	}

	fn handle_event(&mut self, event: SchedulerEvent) {
		debug!("scheduler event: {event:?}");

		match event.action {
			EventTy::Unblock => self.unblock(event.tid),
		}
	}
}

#[cfg(test)]
mod tests {
	use alloc::collections::BTreeMap;
	use super::*;

	#[test]
	fn btree_dup_key() {
		let mut tree = BTreeMap::from([(1, true), (2, false), (3, true)]);
		assert_eq!(tree.get_many_mut([&1, &1]), Err(DuplicateKey));
	}
}
