pub mod cpu;
pub mod msr;

use crate::constants::POWER_SCALE;

/// Calculates power in microwatts from energy readings
///
/// # Arguments
///
/// * `energy_start` - Starting energy reading
/// * `energy_end` - Ending energy reading
/// * `time_interval_ms` - Time interval in milliseconds
/// * `energy_unit` - Energy unit from MSR (power of 2)
///
/// # Returns
///
/// Power in microwatts
pub const fn calculate_power_uw(energy_start: u64, energy_end: u64, time_interval_ms: u64, energy_unit: u64) -> u64 {
	let energy_difference = if energy_end < energy_start {
		// Handle counter wrap-around
		energy_end + 0xFFFF_FFFF - energy_start
	} else {
		energy_end - energy_start
	};

	// Convert to microjoules based on the energy unit
	let energy_uj = (energy_difference * POWER_SCALE) >> energy_unit;

	// Convert to microwatts (µJ/ms = µW)
	energy_uj * 1000 / time_interval_ms
}
