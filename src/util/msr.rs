use msru::{Accessor, Msr};
use std::io;

/// Reads a value from a Model-Specific Register (MSR)
///
/// # Arguments
///
/// * `msr_address` - The address of the MSR to read
/// * `cpu_id` - The CPU ID to read the MSR from
///
/// # Returns
///
/// The value read from the MSR, or an io::Error if the read fails
pub fn read_msr(msr_address: u32, cpu_id: usize) -> io::Result<u64> {
	Msr::new(msr_address, cpu_id as u16)
		.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
		.read()
		.map_err(|e| io::Error::new(io::ErrorKind::Other, e))
}
