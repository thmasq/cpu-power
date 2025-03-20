use std::collections::HashMap;
use std::fmt::Debug;
use std::io;

use crate::cpu_type::{CoreType, CpuType};
use crate::energy::EnergySnapshot;
use crate::mapper::intel::IntelCoreMapper;
use crate::mapper::{CoreMapper, create_core_mapper};

/// Represents the CPU topology including core and thread relationships
pub struct CpuTopology {
	/// The CPU manufacturer type
	pub cpu_type: CpuType,

	/// Number of physical cores
	pub physical_cores: usize,

	/// Maps physical core ID to a list of its logical processors (threads) and core type
	pub core_to_threads: HashMap<usize, (Vec<usize>, CoreType)>,

	/// Maps logical processor ID to its physical core ID and core type
	pub thread_to_core: HashMap<usize, (usize, CoreType)>,

	/// The mapper responsible for this CPU type
	pub mapper: Box<dyn CoreMapper>,
}

impl Clone for CpuTopology {
	fn clone(&self) -> Self {
		Self {
			cpu_type: self.cpu_type,
			physical_cores: self.physical_cores,
			core_to_threads: self.core_to_threads.clone(),
			thread_to_core: self.thread_to_core.clone(),
			mapper: self.mapper.clone_box(),
		}
	}
}

impl Debug for CpuTopology {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("CpuTopology")
			.field("cpu_type", &self.cpu_type)
			.field("physical_cores", &self.physical_cores)
			.field("core_to_threads", &self.core_to_threads)
			.field("thread_to_core", &self.thread_to_core)
			.finish()
	}
}

impl CpuTopology {
	/// Creates a new CpuTopology instance by detecting the system's CPU configuration
	pub fn new() -> io::Result<Self> {
		let mapper = create_core_mapper();
		let cpu_type = mapper.get_cpu_type();

		let (core_to_threads, thread_to_core) = mapper.map_threads_to_cores()?;

		Ok(Self {
			cpu_type,
			physical_cores: core_to_threads.len(),
			core_to_threads,
			thread_to_core,
			mapper,
		})
	}

	/// Reads a snapshot of energy values from MSRs
	pub fn read_energy_snapshot(&self) -> io::Result<EnergySnapshot> {
		self.mapper.read_energy_snapshot(&self.core_to_threads)
	}

	/// Gets the energy unit value from MSRs
	pub fn get_energy_unit(&self) -> io::Result<u64> {
		self.mapper.get_energy_unit()
	}

	/// Estimates core powers using Intel's mapper (only for Intel CPUs)
	pub fn estimate_core_powers(&self, mapper: &mut IntelCoreMapper, pkg_power: u64) -> HashMap<usize, u64> {
		mapper.estimate_core_powers(pkg_power, &self.core_to_threads, &self.thread_to_core)
	}

	/// Returns a list of core types present in the system
	pub fn get_core_types(&self) -> Vec<CoreType> {
		let mut types = std::collections::HashSet::new();
		for (_, core_type) in self.core_to_threads.values() {
			types.insert(*core_type);
		}
		types.into_iter().collect()
	}

	/// Count cores of each type
	pub fn count_core_types(&self) -> HashMap<CoreType, usize> {
		let mut counts = HashMap::new();

		for (_, core_type) in self.core_to_threads.values() {
			*counts.entry(*core_type).or_insert(0) += 1;
		}

		counts
	}
}
