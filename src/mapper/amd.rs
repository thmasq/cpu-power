use std::collections::HashMap;
use std::io;

use crate::constants::*;
use crate::cpu_type::{CoreType, CpuType};
use crate::energy::EnergySnapshot;
use crate::mapper::{CoreMapper, read_topology_from_sysfs};
use crate::util::msr::read_msr;

/// AMD-specific implementation for thread-to-core mapping and energy reading
#[derive(Debug, Clone)]
pub struct AmdCoreMapper;

impl CoreMapper for AmdCoreMapper {
	fn get_cpu_type(&self) -> CpuType {
		CpuType::Amd
	}

	fn map_threads_to_cores(
		&self,
	) -> io::Result<(
		HashMap<usize, (Vec<usize>, CoreType)>,
		HashMap<usize, (usize, CoreType)>,
	)> {
		// Try to use sysfs first for accurate information
		if let Ok(mappings) = read_topology_from_sysfs() {
			return Ok(mappings);
		}

		// Fall back to the AMD thread-to-core layout algorithm
		let mut core_to_threads: HashMap<usize, (Vec<usize>, CoreType)> = HashMap::new();
		let mut thread_to_core: HashMap<usize, (usize, CoreType)> = HashMap::new();

		let total_threads = num_cpus::get();
		let physical_cores = num_cpus::get_physical();
		let threads_per_core = if physical_cores > 0 {
			total_threads / physical_cores
		} else {
			1
		};

		// AMD typically maps like this for 2 threads per core:
		// Core 0: Thread 0, Thread 1
		// Core 1: Thread 2, Thread 3
		// etc.
		for thread_id in 0..total_threads {
			// For AMD with SMT, core_id = thread_id / threads_per_core
			let core_id = thread_id / threads_per_core;

			// AMD doesn't have hybrid architecture currently, so mark all as unknown
			let core_type = CoreType::Unknown;

			core_to_threads
				.entry(core_id)
				.and_modify(|(threads, _)| threads.push(thread_id))
				.or_insert_with(|| (vec![thread_id], core_type));

			thread_to_core.insert(thread_id, (core_id, core_type));
		}

		Ok((core_to_threads, thread_to_core))
	}

	fn read_energy_snapshot(
		&self,
		core_to_threads: &HashMap<usize, (Vec<usize>, CoreType)>,
	) -> io::Result<EnergySnapshot> {
		let mut cores = HashMap::new();

		// AMD: Energy MSRs are available per-core, try to read from first thread of each core
		for (&core_id, (threads, _)) in core_to_threads {
			if let Some(&first_thread) = threads.first() {
				if let Ok(energy) = read_msr(AMD_ENERGY_CORE_MSR, first_thread) {
					cores.insert(core_id, energy);
				}
			}
		}

		let package = read_msr(AMD_ENERGY_PKG_MSR, 0)?;
		Ok(EnergySnapshot {
			package,
			cores,
			estimated: false,
		})
	}

	fn get_energy_unit(&self) -> io::Result<u64> {
		let unit_msr = read_msr(AMD_ENERGY_UNIT_MSR, 0)?;
		Ok((unit_msr >> 8) & 0x1F)
	}

	fn clone_box(&self) -> Box<dyn CoreMapper> {
		Box::new(self.clone())
	}
}
