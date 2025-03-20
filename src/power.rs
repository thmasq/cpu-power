use std::collections::HashMap;

use crate::cpu_type::CoreType;

/// Contains power readings for the entire CPU package and individual cores
#[derive(Debug)]
pub struct PowerReading {
	/// Total package power in watts
	pub package: f64,

	/// Per-core power readings with core types
	/// Maps physical core ID -> (power reading in watts, core type)
	pub cores: HashMap<usize, (f64, CoreType)>,

	/// Whether these values are estimated (true) or directly measured (false)
	pub estimated: bool,
}
