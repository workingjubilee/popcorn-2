use core::arch::asm;
use core::arch::x86_64::{__cpuid, CpuidResult};
use core::num::NonZeroU128;
use log::debug;
use kernel_api::sync::OnceLock;

#[export_name = "__popcorn_system_time"]
pub(crate) fn system_time() -> u128 {
	static TSC_MULTIPLIER: OnceLock<(u128, NonZeroU128)> = OnceLock::new();

	let low: u32;
	let high: u32;
	unsafe {
		asm!("rdtsc", out("eax") low, out("edx") high, options(nostack, preserves_flags, nomem));
	}
	let tsc_val = (low as u128) | (high as u128) << 32;
	let (num, denom) = TSC_MULTIPLIER.get_or_init(|| {
		let mut multiplier = None::<(u128, NonZeroU128)>;

		let max_leaf = unsafe { __cpuid(0) }.eax;
		if max_leaf >= 0x15 {
			debug!("[TSC] Leaf 15h supported");
			let CpuidResult { eax: denom, ebx: num, ecx: freq, .. } = unsafe { __cpuid(0x15) };
			if let Some(num) = NonZeroU128::new(num.into()) && let Some(freq) = NonZeroU128::new(freq.into()) {
				debug!("[TSC] Leaf 15h enumerated");
				multiplier = Some((1000000000 * u128::from(denom), num.checked_mul(freq).unwrap()));
			}
		} else {
			let is_virtualised = (unsafe { __cpuid(0x1) }.ecx & (1 << 31)) != 0;
			if is_virtualised {
				debug!("[TSC] Is virtualized");
				let max_leaf = unsafe { __cpuid(0x40000000) }.eax;
				if max_leaf >= 0x40000010 {
					debug!("[TSC] Leaf 40000010h supported");
					let CpuidResult { eax: freq, .. } = unsafe { __cpuid(0x40000010) };
					if let Some(freq) = NonZeroU128::new(freq.into()) {
						debug!("[TSC] Leaf 40000010h enumerated: {freq}kHz");
						multiplier = Some((1000000, freq))
					}
				}
			}
		}
		
		debug!("multiplier is {multiplier:?}");

		multiplier.unwrap_or_else(|| panic!("Unable to determine TSC frequency"))
	});

	tsc_val * num / denom.get()
}
