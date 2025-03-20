pub mod amd;
pub mod intel;

use std::collections::HashMap;
use std::fmt::Debug;
use std::{fs, io};

use crate::cpu_type::{CoreType, CpuType, detect_core_type};
use crate::energy::EnergySnapshot;

/// Trait for different mapping strategies between logical processors and physical cores
pub trait CoreMapper: Debug + Send + Sync {
	/// Maps logical threads to physical cores
	///
	/// Returns:
	/// - A map from core ID to (list of thread IDs, core type)
	/// - A map from thread ID to (core ID, core type)
	fn map_threads_to_cores(
		&self,
	) -> io::Result<(
		HashMap<usize, (Vec<usize>, CoreType)>,
		HashMap<usize, (usize, CoreType)>,
	)>;

	/// Returns the CPU type (Intel, AMD, etc.)
	fn get_cpu_type(&self) -> CpuType;

	/// Reads energy values from MSRs
	fn read_energy_snapshot(
		&self,
		core_to_threads: &HashMap<usize, (Vec<usize>, CoreType)>,
	) -> io::Result<EnergySnapshot>;

	/// Gets the energy unit value from MSRs
	fn get_energy_unit(&self) -> io::Result<u64>;

	/// Clone implementation for trait objects
	fn clone_box(&self) -> Box<dyn CoreMapper>;
}

/// Helper function to read topology from sysfs, including core type detection
///
/// This reads the CPU topology directly from the Linux sysfs filesystem,
/// which provides the most accurate mapping between logical processors
/// and physical cores.
pub fn read_topology_from_sysfs() -> io::Result<(
	HashMap<usize, (Vec<usize>, CoreType)>,
	HashMap<usize, (usize, CoreType)>,
)> {
	let mut core_to_threads: HashMap<usize, (Vec<usize>, CoreType)> = HashMap::new();
	let mut thread_to_core: HashMap<usize, (usize, CoreType)> = HashMap::new();

	if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu/") {
		for entry in entries.filter_map(Result::ok) {
			let path = entry.path();
			let filename = path.file_name().unwrap_or_default().to_string_lossy();

			// Look for cpuN directories
			if filename.starts_with("cpu") && filename[3..].parse::<usize>().is_ok() {
				let cpu_id = filename[3..].parse::<usize>().unwrap();

				// Read physical core ID from topology/core_id
				let core_id_path = path.join("topology/core_id");

				if let Ok(core_id_str) = fs::read_to_string(&core_id_path) {
					if let Ok(core_id) = core_id_str.trim().parse::<usize>() {
						// Determine core type (P-core or E-core)
						let core_type = detect_core_type(cpu_id);

						// Add to mappings
						core_to_threads
							.entry(core_id)
							.and_modify(|(threads, existing_type)| {
								threads.push(cpu_id);
								// If we already found this core with a specific type, keep it
								if *existing_type == CoreType::Unknown {
									*existing_type = core_type;
								}
							})
							.or_insert_with(|| (vec![cpu_id], core_type));

						thread_to_core.insert(cpu_id, (core_id, core_type));
					}
				}
			}
		}

		if !core_to_threads.is_empty() {
			return Ok((core_to_threads, thread_to_core));
		}
	}

	Err(io::Error::new(
		io::ErrorKind::NotFound,
		"Could not read CPU topology from sysfs",
	))
}

/// Factory function to create the appropriate mapper based on CPU type
pub fn create_core_mapper() -> Box<dyn CoreMapper> {
	use crate::cpu_type::detect_cpu_type;
	use crate::mapper::amd::AmdCoreMapper;
	use crate::mapper::intel::IntelCoreMapper;

	let cpu_type = detect_cpu_type();
	match cpu_type {
		CpuType::Intel => Box::new(IntelCoreMapper::new()),
		CpuType::Amd => Box::new(AmdCoreMapper {}),
		CpuType::Unsupported => {
			eprintln!("Unsupported CPU type, defaulting to Intel mapping");
			Box::new(IntelCoreMapper::new())
		},
	}
}
