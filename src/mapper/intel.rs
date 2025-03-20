use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use std::{io, thread};

use crate::constants::*;
use crate::cpu_type::{CoreType, CpuType, detect_core_type};
use crate::energy::EnergySnapshot;
use crate::mapper::{CoreMapper, read_topology_from_sysfs};
use crate::topology::CpuTopology;
use crate::util::calculate_power_uw;
use crate::util::cpu::CpuUtilization;
use crate::util::msr::read_msr;

/// Intel-specific implementation with hybrid-aware power estimation
#[derive(Debug, Clone)]
pub struct IntelCoreMapper {
	// Tracking min/max power for the entire package
	min_pkg_power: Option<u64>,
	max_pkg_power: Option<u64>,

	// Separate idle power tracking for different core types
	idle_pkg_power: Option<u64>,
	idle_core_power: HashMap<usize, u64>, // Per-core idle power
	core_types: HashMap<usize, CoreType>, // Track which core is which type

	// Baseline power per core type
	pcore_idle_power: Option<u64>,
	ecore_idle_power: Option<u64>,

	utilization: CpuUtilization,
}

impl IntelCoreMapper {
	pub fn new() -> Self {
		Self {
			min_pkg_power: None,
			max_pkg_power: None,
			idle_pkg_power: None,
			idle_core_power: HashMap::new(),
			core_types: HashMap::new(),
			pcore_idle_power: None,
			ecore_idle_power: None,
			utilization: CpuUtilization::new(),
		}
	}

	/// Calibrates to find idle power consumption for different core types
	pub fn calibrate(&mut self, topology: &CpuTopology) -> io::Result<()> {
		println!("Calibrating idle power consumption for different core types...");

		// Get energy unit for power calculation
		let energy_unit = self.get_energy_unit()?;

		// Store core types for later reference
		for (core_id, (_, core_type)) in &topology.core_to_threads {
			self.core_types.insert(*core_id, *core_type);
		}

		// Count core types
		let mut pcore_count = 0;
		let mut ecore_count = 0;
		let mut unknown_core_count = 0;

		for core_type in self.core_types.values() {
			match core_type {
				CoreType::PCore => pcore_count += 1,
				CoreType::ECore => ecore_count += 1,
				CoreType::Unknown => unknown_core_count += 1,
			}
		}

		println!(
			"Detected core composition: {} P-cores, {} E-cores, {} unknown cores",
			pcore_count, ecore_count, unknown_core_count
		);

		// First measure baseline system power without any active calibration
		let baseline_start = self.read_energy_snapshot(&topology.core_to_threads)?;
		thread::sleep(Duration::from_millis(100));
		let baseline_end = self.read_energy_snapshot(&topology.core_to_threads)?;

		// Calculate baseline power (includes OS background activity)
		let baseline_power = calculate_power_uw(baseline_start.package, baseline_end.package, 100, energy_unit);
		println!(
			"System baseline power: {:.2} W",
			baseline_power as f64 / POWER_SCALE as f64
		);

		// Calibrate P-cores if available
		if pcore_count > 0 {
			self.calibrate_core_type(topology, CoreType::PCore, energy_unit, baseline_power)?;
		}

		// Calibrate E-cores if available
		if ecore_count > 0 {
			self.calibrate_core_type(topology, CoreType::ECore, energy_unit, baseline_power)?;
		}

		// Calibrate unknown cores if that's all we have
		if pcore_count == 0 && ecore_count == 0 && unknown_core_count > 0 {
			self.calibrate_core_type(topology, CoreType::Unknown, energy_unit, baseline_power)?;
		}

		// Calculate total idle package power
		let mut total_idle_power = 0;

		for (core_id, core_type) in &self.core_types {
			let core_idle_power = match core_type {
				CoreType::PCore => self.pcore_idle_power,
				CoreType::ECore => self.ecore_idle_power,
				CoreType::Unknown => self.pcore_idle_power, // Fall back to P-core values for unknown
			};

			if let Some(power) = core_idle_power {
				self.idle_core_power.insert(*core_id, power);
				total_idle_power += power;
			}
		}

		self.idle_pkg_power = Some(total_idle_power);

		println!(
			"Calibration complete. Estimated idle package power: {:.2} W",
			total_idle_power as f64 / POWER_SCALE as f64
		);

		if let Some(power) = self.pcore_idle_power {
			println!(
				"  P-core idle power: {:.2} W per core",
				power as f64 / POWER_SCALE as f64
			);
		}

		if let Some(power) = self.ecore_idle_power {
			println!(
				"  E-core idle power: {:.2} W per core",
				power as f64 / POWER_SCALE as f64
			);
		}

		Ok(())
	}

	/// Calibrates a specific core type (P-core or E-core) by running a controlled workload
	fn calibrate_core_type(
		&mut self,
		topology: &CpuTopology,
		core_type: CoreType,
		energy_unit: u64,
		baseline_power: u64,
	) -> io::Result<()> {
		println!("Calibrating {} idle power...", core_type.as_str());

		// Find a core of the specified type to use for calibration
		let mut calibration_core_id = None;
		let mut calibration_thread_id = None;

		for (core_id, (threads, ctype)) in &topology.core_to_threads {
			if *ctype == core_type && !threads.is_empty() {
				calibration_core_id = Some(*core_id);
				calibration_thread_id = Some(threads[0]);
				break;
			}
		}

		let (calibration_core_id, calibration_thread_id) = match (calibration_core_id, calibration_thread_id) {
			(Some(core), Some(thread)) => (core, thread),
			_ => {
				return Err(io::Error::new(
					io::ErrorKind::NotFound,
					format!("No {} found for calibration", core_type.as_str()),
				));
			},
		};

		println!(
			"Using {} ID: {} (Thread ID: {}) for calibration",
			core_type.as_str(),
			calibration_core_id,
			calibration_thread_id
		);

		// Count how many cores of this type we have
		let type_count = topology
			.core_to_threads
			.values()
			.filter(|(_, c)| *c == core_type)
			.count();

		// Channel to signal the calibration thread to stop
		let (tx, rx) = mpsc::channel();

		// Atomic flag to control the calibration thread
		let running = Arc::new(AtomicBool::new(true));
		let thread_running = running.clone();

		// Spawn a calibration thread on the selected core
		let handle = thread::spawn(move || {
			// Try to pin to selected CPU using Linux-specific API
			unsafe {
				let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
				libc::CPU_SET(calibration_thread_id, &mut cpuset);

				// Get the pthread_t of the current thread
				let thread_id = libc::pthread_self();

				let result = libc::pthread_setaffinity_np(thread_id, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);

				if result != 0 {
					eprintln!("Warning: Failed to set thread affinity");
				}
			}

			// Wait for signal to start actual measurement
			let _ = rx.recv_timeout(Duration::from_millis(200));

			// Run a sequence of NOP instructions in a tight loop
			while thread_running.load(Ordering::Relaxed) {
				// Execute a batch of NOP instructions using inline assembly
				for _ in 0..1_000_000 {
					#[cfg(target_arch = "x86_64")]
					unsafe {
						std::arch::asm!("nop", "nop", "nop", "nop", "nop", "nop", "nop", "nop");
					}
				}

				if !thread_running.load(Ordering::Relaxed) {
					break;
				}
			}
		});

		// Give the thread time to start and stabilize
		thread::sleep(Duration::from_millis(200));

		// Signal to start the actual measurement
		let _ = tx.send(());

		// Measure power during NOP execution
		let initial_snapshot = self.read_energy_snapshot(&topology.core_to_threads)?;
		thread::sleep(Duration::from_millis(1000));
		let final_snapshot = self.read_energy_snapshot(&topology.core_to_threads)?;

		// Stop the calibration thread
		running.store(false, Ordering::Relaxed);
		let _ = handle.join();

		// Calculate power during NOP execution
		let nop_power = calculate_power_uw(initial_snapshot.package, final_snapshot.package, 1000, energy_unit);

		// The difference is what our NOP loop added beyond baseline
		let single_core_min_power = if nop_power > baseline_power {
			(nop_power - baseline_power) / type_count as u64
		} else {
			// Fallback if measurement is inconsistent
			nop_power / type_count as u64
		};

		// Store the calibrated power for this core type
		match core_type {
			CoreType::PCore => self.pcore_idle_power = Some(single_core_min_power),
			CoreType::ECore => self.ecore_idle_power = Some(single_core_min_power),
			CoreType::Unknown => {
				// For unknown cores, set both P and E to the same value
				self.pcore_idle_power = Some(single_core_min_power);
				self.ecore_idle_power = Some(single_core_min_power);
			},
		}

		println!(
			"{} calibration complete. Estimated idle power: {:.2} W per core",
			core_type.as_str(),
			single_core_min_power as f64 / POWER_SCALE as f64
		);

		Ok(())
	}

	/// Updates power bounds based on new reading
	pub fn update_power_bounds(&mut self, pkg_power: u64) {
		// Update min power
		match self.min_pkg_power {
			None => self.min_pkg_power = Some(pkg_power),
			Some(min) if pkg_power < min => self.min_pkg_power = Some(pkg_power),
			_ => {},
		}

		// Update max power
		match self.max_pkg_power {
			None => self.max_pkg_power = Some(pkg_power),
			Some(max) if pkg_power > max => self.max_pkg_power = Some(pkg_power),
			_ => {},
		}
	}

	/// Estimates per-core power based on utilization and package power
	///
	/// This accounts for different core types (P-cores vs E-cores) when distributing power.
	pub fn estimate_core_powers(
		&mut self,
		pkg_power: u64,
		core_to_threads: &HashMap<usize, (Vec<usize>, CoreType)>,
		thread_to_core: &HashMap<usize, (usize, CoreType)>,
	) -> HashMap<usize, u64> {
		// Update CPU utilization
		let _ = self.utilization.update();

		// Get per-core utilization
		let core_utils = self.utilization.get_core_utilization(thread_to_core);

		// Update min/max power bounds
		self.update_power_bounds(pkg_power);

		// Use calibrated idle power if available, otherwise fall back to observed minimum
		let idle_power = self.idle_pkg_power.unwrap_or_else(|| self.min_pkg_power.unwrap_or(0));
		let dynamic_power = pkg_power.saturating_sub(idle_power);

		let mut core_powers = HashMap::new();

		// First pass: calculate weighted utilization based on core type
		let mut weighted_utils = HashMap::new();
		let mut total_weighted_util = 0.0;

		for (&core_id, &util) in &core_utils {
			if let Some((_, core_type)) = core_to_threads.get(&core_id) {
				// Apply weight based on core type (P-cores consume more power than E-cores)
				let weight = match core_type {
					CoreType::PCore => 3.0,   // P-cores typically use 3-4x more power
					CoreType::ECore => 1.0,   // Base weight for E-cores
					CoreType::Unknown => 2.0, // Middle ground for unknown cores
				};

				let weighted_util = util * weight;
				weighted_utils.insert(core_id, weighted_util);
				total_weighted_util += weighted_util;
			}
		}

		// Distribute dynamic power based on weighted utilization
		if total_weighted_util > 0.0 {
			for (&core_id, &weighted_util) in &weighted_utils {
				if let Some((_, core_type)) = core_to_threads.get(&core_id) {
					let power_ratio = weighted_util / total_weighted_util;
					let core_dynamic_power = (dynamic_power as f64 * power_ratio) as u64;

					// Get idle power for this core type
					let idle_core_power = match core_type {
						CoreType::PCore => self.pcore_idle_power,
						CoreType::ECore => self.ecore_idle_power,
						CoreType::Unknown => self.pcore_idle_power, // Default to P-core for unknown
					}
					.unwrap_or_else(|| {
						// Fall back to generic idle power
						self.idle_core_power
							.get(&core_id)
							.copied()
							.unwrap_or_else(|| idle_power / core_to_threads.len() as u64)
					});

					core_powers.insert(core_id, idle_core_power + core_dynamic_power);
				}
			}
		} else {
			// If all cores idle, use calibrated idle power values
			for (core_id, (_, core_type)) in core_to_threads {
				let idle_core_power = match core_type {
					CoreType::PCore => self.pcore_idle_power,
					CoreType::ECore => self.ecore_idle_power,
					CoreType::Unknown => self.pcore_idle_power, // Default to P-core for unknown
				}
				.unwrap_or_else(|| {
					// Fall back to generic idle power
					self.idle_core_power
						.get(core_id)
						.copied()
						.unwrap_or_else(|| idle_power / core_to_threads.len() as u64)
				});

				core_powers.insert(*core_id, idle_core_power);
			}
		}

		core_powers
	}
}

impl CoreMapper for IntelCoreMapper {
	fn get_cpu_type(&self) -> CpuType {
		CpuType::Intel
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

		// Fall back to the Intel thread-to-core layout algorithm with core type detection
		let mut core_to_threads: HashMap<usize, (Vec<usize>, CoreType)> = HashMap::new();
		let mut thread_to_core: HashMap<usize, (usize, CoreType)> = HashMap::new();

		let total_threads = num_cpus::get();
		let physical_cores = num_cpus::get_physical();
		let _threads_per_core = if physical_cores > 0 {
			total_threads / physical_cores
		} else {
			1
		};

		// Intel typically maps like this for 2 threads per core:
		// Core 0: Thread 0, Thread physical_cores
		// Core 1: Thread 1, Thread physical_cores+1
		// etc.
		for thread_id in 0..total_threads {
			// Intel's mapping: For a thread t, if t < physical_cores, core = t
			// Otherwise, it's the second thread of core (t - physical_cores)
			let core_id = if thread_id < physical_cores {
				thread_id
			} else {
				// Determine which hyperthread this is
				let _hyperthread_index = thread_id / physical_cores;
				let core_index = thread_id % physical_cores;
				core_index // The physical core ID is just the remainder
			};

			// Try to detect core type
			let core_type = detect_core_type(thread_id);

			core_to_threads
				.entry(core_id)
				.and_modify(|(threads, existing_type)| {
					threads.push(thread_id);
					// If we already found this core with a specific type, keep it
					if *existing_type == CoreType::Unknown {
						*existing_type = core_type;
					}
				})
				.or_insert_with(|| (vec![thread_id], core_type));

			thread_to_core.insert(thread_id, (core_id, core_type));
		}

		Ok((core_to_threads, thread_to_core))
	}

	fn read_energy_snapshot(
		&self,
		_core_to_threads: &HashMap<usize, (Vec<usize>, CoreType)>,
	) -> io::Result<EnergySnapshot> {
		// For Intel, we only read the package energy - core energy will be estimated later
		let package = read_msr(INTEL_PKG_ENERGY_MSR, 0)?;

		// Return empty cores map - we'll estimate values during power calculation
		Ok(EnergySnapshot {
			package,
			cores: HashMap::new(),
			estimated: true,
		})
	}

	fn get_energy_unit(&self) -> io::Result<u64> {
		let unit_msr = read_msr(INTEL_POWER_UNIT_MSR, 0)?;
		Ok((unit_msr >> 8) & 0x1F)
	}

	fn clone_box(&self) -> Box<dyn CoreMapper> {
		Box::new(self.clone())
	}
}
