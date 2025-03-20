use std::collections::HashMap;

/// Snapshot of energy readings from CPU registers
///
/// This structure captures a point-in-time energy reading for the CPU package
/// and individual cores when available.
#[derive(Debug)]
pub struct EnergySnapshot {
	/// Total package energy reading
	pub package: u64,

	/// Per-core energy readings (physical core ID -> energy reading)
	pub cores: HashMap<usize, u64>,

	/// Whether these values are estimated (true) or directly measured (false)
	pub estimated: bool,
}
