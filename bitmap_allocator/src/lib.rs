#![cfg_attr(not(test), no_std)]

#![feature(kernel_allocation_new)]
#![feature(kernel_frame_zero)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use core::num::NonZeroUsize;
use core::ops::Range;
use kernel_api::memory::{Frame, PhysicalAddress, AllocError};
use kernel_api::memory::allocator::{AllocationMeta, BackingAllocator, Config};
use kernel_api::sync::Mutex;
use log::warn;

#[derive(Debug, Eq, PartialEq)]
enum FrameState {
    Allocated,
    Free
}

#[derive(Debug)]
struct OutOfRangeError;

struct BitmapAllocator {
    first_frame: Frame,
    bitmap: Box<[usize]>
}

impl BitmapAllocator {
    fn last_frame(&self) -> Frame {
        self.first_frame + (self.bitmap.len() * mem::size_of::<usize>())
    }

    fn set_frame(&mut self, frame: Frame, state: FrameState) -> Result<(), OutOfRangeError> {
        if (frame < self.first_frame) || (frame >= self.last_frame()) { return Err(OutOfRangeError); }

        let (bitmap_index, bit_index) = self.frame_to_indices(frame);
        match state {
            FrameState::Allocated => self.bitmap[bitmap_index] &= !(1 << bit_index),
            FrameState::Free => self.bitmap[bitmap_index] |= (1 << bit_index),
        }

        Ok(())
    }

    fn get_frame(&self, frame: Frame) -> Result<FrameState, OutOfRangeError> {
        if (frame < self.first_frame) || (frame >= self.last_frame()) { return Err(OutOfRangeError); }

        let (bitmap_index, bit_index) = self.frame_to_indices(frame);
        if (self.bitmap[bitmap_index] & (1 << bit_index)) == 0 { Ok(FrameState::Allocated) }
        else { Ok(FrameState::Free) }
    }

    fn new(first_frame: Frame, frame_count: usize) -> Self {
        let bitmap_length = frame_count / mem::size_of::<usize>();
        let bitmap = Vec::into_boxed_slice(vec![0; bitmap_length]);

        Self {
            first_frame,
            bitmap
        }
    }

    fn frame_to_indices(&self, frame: Frame) -> (usize, usize) {
        assert!(frame >= self.first_frame);

        let number_in_bitmap = frame - self.first_frame;
        let bitmap_index = number_in_bitmap / mem::size_of::<usize>();
        let bit_index = number_in_bitmap % mem::size_of::<usize>();

        (bitmap_index, bit_index)
    }

    fn allocate_one(&mut self) -> Result<Frame, AllocError> {
        for (i, entry) in self.bitmap.iter_mut().enumerate() {
            let first_set_bit = usize::try_from(entry.trailing_zeros()).unwrap();
            if first_set_bit != mem::size_of::<usize>() {
                *entry &= !(1usize << first_set_bit);
                let bits_to_start = i * mem::size_of::<usize>();
                let start = self.first_frame + bits_to_start + first_set_bit;
                return Ok(start);
            }
        }

        return Err(AllocError);
    }

    fn allocate_multiple_fast(&mut self, frame_count: usize) -> Result<Frame, AllocError> {
        assert!(frame_count > 1);

        // Cannot allocate bigger than number of bits in usize since can't check across boundaries
        if frame_count > mem::size_of::<usize>() { return Err(AllocError); }

        // Create a mask of `frame_count` contiguous bits
        let mask = (2 << (frame_count - 1)) - 1;

        for (i, entry) in self.bitmap.iter_mut().enumerate() {
            // locate the first free frame so we don't waste time checking unnecessary bits
            let first_set_bit = usize::try_from(entry.trailing_zeros()).unwrap();

            /*
             We slide the entry along, masking off the number of frames we want, and checking all the frames are free
             We can't slide further than `size_of(usize) - frame_count` as this would mean there can't possibly be enough frames left,
             since less than `frame_count` bits came from the original entry

             `frame_count = 2`
             `mask = 0b00000011`
             `entry = 0b01100100`

             `first_set_bit` is `2`, so we start with `slide = 2`
             `shifted = entry >> slide = 0b00011001`
             Then we mask the entry
             `masked = shifted & mask = 0b00000001`
             If enough frames were free, then the masked result should be equal to the mask, and we can allocate
             If not, we repeat with a larger slide
             */
            for slide in first_set_bit..(mem::size_of::<usize>() - frame_count) {
                let shifted = *entry >> slide;
                let masked = shifted & mask;
                if masked == mask {
                    *entry &= !(mask << slide);
                    let bits_to_start = i * mem::size_of::<usize>();
                    let start = self.first_frame + bits_to_start + slide;
                    return Ok(start);
                }
            }
        }

        Err(AllocError)
    }

    #[cold]
    fn allocate_multiple_slow(&mut self, frame_count: usize) -> Result<Frame, AllocError> {
        warn!("TODO: Check cross-word boundary");
        Err(AllocError)
    }
}

pub struct Wrapped(Mutex<BitmapAllocator>);

unsafe impl BackingAllocator for Wrapped {
    fn allocate_contiguous(&self, frame_count: usize) -> Result<Frame, AllocError> {
        let mut guard = self.0.lock();

        if frame_count == 1 { guard.allocate_one() }
        else {
            guard.allocate_multiple_fast(frame_count)
                    .or_else(|_| guard.allocate_multiple_slow(frame_count))
        }
    }

    unsafe fn deallocate_contiguous(&self, base: Frame, frame_count: NonZeroUsize) {
        todo!()
    }

    fn new(config: Config) -> Arc<dyn BackingAllocator> where Self: Sized {
        let Range { start, end } = config.allocation_range;
        let mut allocator = BitmapAllocator::new(start, end - start);

        for free_region in config.regions {
            for frame in free_region {
                allocator.set_frame(frame, FrameState::Free)
                    .unwrap();
            }
        }

        Arc::new(Wrapped(Mutex::new(allocator)))
    }

    fn push(&mut self, allocation: AllocationMeta) {
        let allocator = self.0.get_mut();

        for frame in allocation.region {
            allocator.set_frame(frame, FrameState::Allocated)
                .unwrap();
        }
    }

    fn drain_into(self, into: &mut dyn BackingAllocator) {
        let allocator = self.0.into_inner();

        for frame in allocator.first_frame..allocator.last_frame() {
            if allocator.get_frame(frame).unwrap() == FrameState::Allocated {
                into.push(AllocationMeta { region: frame .. frame+1 });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


}
