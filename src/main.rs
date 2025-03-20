use libc;
use msru::{Accessor, Msr};
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::io::{self, BufRead, BufReader, Write};
use std::time::{Duration, Instant};
use std::{fs, thread};

// AMD RAPL MSR addresses
const AMD_ENERGY_UNIT_MSR: u32 = 0xC001_0299;
const AMD_ENERGY_CORE_MSR: u32 = 0xC001_029A;
const AMD_ENERGY_PKG_MSR: u32 = 0xC001_029B;

// Intel RAPL MSR addresses
const INTEL_POWER_UNIT_MSR: u32 = 0x606;
const INTEL_PKG_ENERGY_MSR: u32 = 0x611;

// Intel Hybrid Architecture MSRs
const INTEL_CORE_TYPE_MSR: u32 = 0x19A; // CPU capability flags, used to determine core type

const DATA_COLLECTION_INTERVAL_MS: u64 = 100;
const DISPLAY_UPDATE_INTERVAL_MS: u64 = 200;
const AVERAGING_ITERATIONS: usize = 10;
const POWER_SCALE: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum CpuType {
	Intel,
	Amd,
	Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CoreType {
	PCore,   // Performance core
	ECore,   // Efficiency core
	Unknown, // For processors without hybrid architecture or unidentified
}

impl CoreType {
	fn as_str(&self) -> &'static str {
		match self {
			CoreType::PCore => "P-core",
			CoreType::ECore => "E-core",
			CoreType::Unknown => "Core",
		}
	}
}

// Helper function to read topology from sysfs, now including core type detection
fn read_topology_from_sysfs() -> io::Result<(
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

// Detect core type (P-core or E-core) using Intel-specific MSRs
fn detect_core_type(cpu_id: usize) -> CoreType {
	// First check if we're on Intel
	let cpu_type = detect_cpu_type();
	if cpu_type != CpuType::Intel {
		return CoreType::Unknown;
	}

	// Try to read the core type from MSR
	// On Intel hybrid architecture, bit 24 in MSR 0x19A indicates core type
	// (0 for P-core, 1 for E-core)
	match read_msr(INTEL_CORE_TYPE_MSR, cpu_id) {
		Ok(value) => {
			// Check bit 24 (Intel's documented bit for hybrid architecture)
			if (value >> 24) & 1 == 0 {
				CoreType::PCore
			} else {
				CoreType::ECore
			}
		},
		Err(_) => {
			// Fallback method: try to read from sysfs (on newer kernels)
			let sysfs_path = format!("/sys/devices/system/cpu/cpu{}/topology/core_type", cpu_id);
			if let Ok(content) = fs::read_to_string(&sysfs_path) {
				let content = content.trim().to_lowercase();
				if content.contains("performance") || content.contains("p-core") {
					CoreType::PCore
				} else if content.contains("efficiency") || content.contains("e-core") {
					CoreType::ECore
				} else {
					CoreType::Unknown
				}
			} else {
				// If we can't determine the type, assume it's a regular core
				CoreType::Unknown
			}
		},
	}
}

// Trait for different mapping strategies between logical processors and physical cores
trait CoreMapper: Debug {
	fn map_threads_to_cores(
		&self,
	) -> io::Result<(
		HashMap<usize, (Vec<usize>, CoreType)>,
		HashMap<usize, (usize, CoreType)>,
	)>;
	fn get_cpu_type(&self) -> CpuType;
	fn read_energy_snapshot(
		&self,
		core_to_threads: &HashMap<usize, (Vec<usize>, CoreType)>,
	) -> io::Result<EnergySnapshot>;
	fn get_energy_unit(&self) -> io::Result<u64>;
	fn clone_box(&self) -> Box<dyn CoreMapper>;
}

// CPU Utilization tracker
#[derive(Debug, Clone)]
struct CpuUtilization {
	prev_stats: HashMap<usize, CpuStats>,
	utilization: HashMap<usize, f64>,
}

#[derive(Debug, Clone, Copy)]
struct CpuStats {
	user: u64,
	nice: u64,
	system: u64,
	idle: u64,
	iowait: u64,
	irq: u64,
	softirq: u64,
	steal: u64,
	total: u64,
}

impl CpuUtilization {
	fn new() -> Self {
		Self {
			prev_stats: HashMap::new(),
			utilization: HashMap::new(),
		}
	}

	fn update(&mut self) -> io::Result<()> {
		// Read /proc/stat for CPU utilization data
		let file = fs::File::open("/proc/stat")?;
		let reader = BufReader::new(file);

		let mut new_stats = HashMap::new();

		for line in reader.lines() {
			let line = line?;
			if line.starts_with("cpu") && !line.starts_with("cpu ") {
				let parts: Vec<&str> = line.split_whitespace().collect();
				if parts.len() >= 8 {
					// Extract CPU ID from "cpuN"
					if let Ok(cpu_id) = parts[0][3..].parse::<usize>() {
						let stats = CpuStats {
							user: parts[1].parse().unwrap_or(0),
							nice: parts[2].parse().unwrap_or(0),
							system: parts[3].parse().unwrap_or(0),
							idle: parts[4].parse().unwrap_or(0),
							iowait: parts[5].parse().unwrap_or(0),
							irq: parts[6].parse().unwrap_or(0),
							softirq: parts[7].parse().unwrap_or(0),
							steal: if parts.len() > 8 {
								parts[8].parse().unwrap_or(0)
							} else {
								0
							},
							total: 0, // Will calculate below
						};

						// Calculate total
						let total = stats.user
							+ stats.nice + stats.system
							+ stats.idle + stats.iowait
							+ stats.irq + stats.softirq
							+ stats.steal;

						new_stats.insert(cpu_id, CpuStats { total, ..stats });
					}
				}
			}
		}

		// Calculate utilization by comparing with previous values
		for (cpu_id, current) in &new_stats {
			if let Some(prev) = self.prev_stats.get(cpu_id) {
				let total_diff = current.total.saturating_sub(prev.total);
				if total_diff > 0 {
					let idle_diff = current.idle.saturating_sub(prev.idle) + current.iowait.saturating_sub(prev.iowait);

					let utilization = 1.0 - (idle_diff as f64 / total_diff as f64);
					self.utilization.insert(*cpu_id, utilization);
				}
			} else {
				// Default to 0% utilization for first reading
				self.utilization.insert(*cpu_id, 0.0);
			}
		}

		// Update previous stats for next iteration
		self.prev_stats = new_stats;

		Ok(())
	}

	fn get_core_utilization(&self, thread_to_core: &HashMap<usize, (usize, CoreType)>) -> HashMap<usize, f64> {
		let mut core_utils = HashMap::new();

		// Combine thread utilizations for each core
		for (thread_id, utilization) in &self.utilization {
			if let Some(&(core_id, _)) = thread_to_core.get(thread_id) {
				let entry = core_utils.entry(core_id).or_insert((0.0, 0));
				entry.0 += *utilization;
				entry.1 += 1;
			}
		}

		// Calculate average utilization per core
		core_utils
			.into_iter()
			.map(|(core_id, (total_util, count))| (core_id, total_util / count as f64))
			.collect()
	}
}

// Intel with hybrid-aware estimation approach
#[derive(Debug, Clone)]
struct IntelCoreMapper {
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
	fn new() -> Self {
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

	// Calibrate to find idle power consumption for both P-cores and E-cores
	fn calibrate(&mut self, topology: &CpuTopology) -> io::Result<()> {
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

	// Calibrate a specific core type (P-core or E-core)
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
		let (tx, rx) = std::sync::mpsc::channel();

		// Atomic flag to control the calibration thread
		let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
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
			while thread_running.load(std::sync::atomic::Ordering::Relaxed) {
				// Execute a batch of NOP instructions using inline assembly
				for _ in 0..1_000_000 {
					#[cfg(target_arch = "x86_64")]
					unsafe {
						std::arch::asm!("nop", "nop", "nop", "nop", "nop", "nop", "nop", "nop");
					}
				}

				if !thread_running.load(std::sync::atomic::Ordering::Relaxed) {
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
		running.store(false, std::sync::atomic::Ordering::Relaxed);
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

	// Update power bounds based on new reading
	fn update_power_bounds(&mut self, pkg_power: u64) {
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

	// Estimate per-core power based on utilization and package power, accounting for different core
	// types
	fn estimate_core_powers(
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

// AMD-specific implementation for thread-to-core mapping
#[derive(Debug, Clone)]
struct AmdCoreMapper;

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

// Factory function to create the appropriate mapper
fn create_core_mapper() -> Box<dyn CoreMapper> {
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

struct CpuTopology {
	cpu_type: CpuType,
	physical_cores: usize,
	// Maps physical core ID to a list of its logical processors (threads) and core type
	core_to_threads: HashMap<usize, (Vec<usize>, CoreType)>,
	// Maps logical processor ID to its physical core ID and core type
	thread_to_core: HashMap<usize, (usize, CoreType)>,
	// The mapper responsible for this CPU type
	mapper: Box<dyn CoreMapper>,
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
	fn new() -> io::Result<Self> {
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

	fn read_energy_snapshot(&self) -> io::Result<EnergySnapshot> {
		self.mapper.read_energy_snapshot(&self.core_to_threads)
	}

	fn get_energy_unit(&self) -> io::Result<u64> {
		self.mapper.get_energy_unit()
	}

	fn estimate_core_powers(&self, mapper: &mut IntelCoreMapper, pkg_power: u64) -> HashMap<usize, u64> {
		mapper.estimate_core_powers(pkg_power, &self.core_to_threads, &self.thread_to_core)
	}

	// Returns a list of core types present in the system
	#[allow(dead_code)]
	fn get_core_types(&self) -> Vec<CoreType> {
		let mut types = std::collections::HashSet::new();
		for (_, core_type) in self.core_to_threads.values() {
			types.insert(*core_type);
		}
		types.into_iter().collect()
	}

	// Count cores of each type
	fn count_core_types(&self) -> HashMap<CoreType, usize> {
		let mut counts = HashMap::new();

		for (_, core_type) in self.core_to_threads.values() {
			*counts.entry(*core_type).or_insert(0) += 1;
		}

		counts
	}
}

#[derive(Debug)]
struct PowerReading {
	package: f64,
	cores: HashMap<usize, (f64, CoreType)>, // Physical core ID -> (power reading, core type)
	estimated: bool,
}

struct EnergySnapshot {
	package: u64,
	cores: HashMap<usize, u64>, // Physical core ID -> energy reading
	estimated: bool,
}

struct PowerMonitor {
	topology: CpuTopology,
	power_readings: VecDeque<u64>,
	core_power_readings: HashMap<usize, VecDeque<u64>>, // Physical core ID -> readings
	last_display_time: Instant,
	intel_mapper: Option<IntelCoreMapper>, // Only used for Intel estimation
	estimated: bool,
}

impl PowerMonitor {
	fn new(topology: CpuTopology) -> Self {
		let mut core_power_readings = HashMap::new();
		for &core_id in topology.core_to_threads.keys() {
			core_power_readings.insert(core_id, VecDeque::with_capacity(AVERAGING_ITERATIONS));
		}

		// Create Intel mapper for estimation if needed
		let mut intel_mapper = if topology.cpu_type == CpuType::Intel {
			Some(IntelCoreMapper::new())
		} else {
			None
		};

		// Run calibration for Intel CPUs
		if let Some(ref mut mapper) = intel_mapper {
			if let Err(e) = mapper.calibrate(&topology) {
				eprintln!("Warning: Calibration failed: {}. Using dynamic calibration instead.", e);
			}
		}

		let estimated = topology.cpu_type == CpuType::Intel;

		Self {
			topology,
			power_readings: VecDeque::with_capacity(AVERAGING_ITERATIONS),
			core_power_readings,
			last_display_time: Instant::now(),
			intel_mapper,
			estimated,
		}
	}

	fn update_readings(&mut self, package_power: u64, core_powers: &HashMap<usize, u64>) {
		self.power_readings.push_back(package_power);
		if self.power_readings.len() > AVERAGING_ITERATIONS {
			self.power_readings.pop_front();
		}

		for (&core_id, &power) in core_powers.iter() {
			if let Some(readings) = self.core_power_readings.get_mut(&core_id) {
				readings.push_back(power);
				if readings.len() > AVERAGING_ITERATIONS {
					readings.pop_front();
				}
			}
		}
	}

	fn calculate_averages(&self) -> PowerReading {
		let package_avg = self.calculate_average_power(&self.power_readings);
		let mut cores = HashMap::new();

		for (&core_id, readings) in &self.core_power_readings {
			if let Some((_, core_type)) = self.topology.core_to_threads.get(&core_id) {
				cores.insert(core_id, (self.calculate_average_power(readings), *core_type));
			}
		}

		PowerReading {
			package: package_avg,
			cores,
			estimated: self.estimated,
		}
	}

	fn calculate_average_power(&self, readings: &VecDeque<u64>) -> f64 {
		if readings.is_empty() {
			return 0.0;
		}
		let total: u64 = readings.iter().sum();
		total as f64 / readings.len() as f64 / POWER_SCALE as f64
	}

	fn should_update_display(&self) -> bool {
		self.last_display_time.elapsed().as_millis() >= u128::from(DISPLAY_UPDATE_INTERVAL_MS)
	}
}

fn detect_cpu_type() -> CpuType {
	let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
	if cpuinfo.contains("GenuineIntel") {
		CpuType::Intel
	} else if cpuinfo.contains("AuthenticAMD") {
		CpuType::Amd
	} else {
		CpuType::Unsupported
	}
}

fn read_msr(msr_address: u32, cpu_id: usize) -> io::Result<u64> {
	Msr::new(msr_address, cpu_id as u16)
		.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
		.read()
		.map_err(|e| io::Error::new(io::ErrorKind::Other, e))
}

const fn calculate_power_uw(energy_start: u64, energy_end: u64, time_interval_ms: u64, energy_unit: u64) -> u64 {
	let energy_difference = if energy_end < energy_start {
		energy_end + 0xFFFF_FFFF - energy_start
	} else {
		energy_end - energy_start
	};

	let energy_uj = (energy_difference * POWER_SCALE) >> energy_unit;
	energy_uj * 1000 / time_interval_ms
}

fn display_power_readings(readings: &PowerReading, topology: &CpuTopology) -> io::Result<()> {
	let _ = topology;
	// Group cores by type for better display
	let mut pcore_list = Vec::new();
	let mut ecore_list = Vec::new();
	let mut unknown_core_list = Vec::new();

	for (&core_id, &(power, core_type)) in &readings.cores {
		match core_type {
			CoreType::PCore => pcore_list.push((core_id, power)),
			CoreType::ECore => ecore_list.push((core_id, power)),
			CoreType::Unknown => unknown_core_list.push((core_id, power)),
		}
	}

	// Sort by core ID
	pcore_list.sort_by_key(|&(id, _)| id);
	ecore_list.sort_by_key(|&(id, _)| id);
	unknown_core_list.sort_by_key(|&(id, _)| id);

	// Calculate total lines needed
	let pcore_rows = (pcore_list.len() + 1) / 2;
	let ecore_rows = (ecore_list.len() + 1) / 2;
	let unknown_rows = (unknown_core_list.len() + 1) / 2;

	// Add headers for each section if present
	let header_count = (if !pcore_list.is_empty() { 1 } else { 0 })
		+ (if !ecore_list.is_empty() { 1 } else { 0 })
		+ (if !unknown_core_list.is_empty() && (pcore_list.is_empty() || ecore_list.is_empty()) {
			1
		} else {
			0
		});

	let total_lines = 1 + pcore_rows + ecore_rows + unknown_rows + header_count;

	print!("\x1B[{total_lines}A"); // Move cursor up

	// Calculate totals by core type
	let pcore_total: f64 = pcore_list.iter().map(|&(_, power)| power).sum();
	let ecore_total: f64 = ecore_list.iter().map(|&(_, power)| power).sum();
	let unknown_total: f64 = unknown_core_list.iter().map(|&(_, power)| power).sum();

	// Display package power
	print!("\x1B[2K");
	println!(
		"Package: {:6.2} W | Cores Total: {:6.2} W {}",
		readings.package,
		pcore_total + ecore_total + unknown_total,
		if readings.estimated { "(Estimated)" } else { "" }
	);

	// Display P-cores if present
	if !pcore_list.is_empty() {
		print!("\x1B[2K");
		println!("Performance Cores: {:6.2} W", pcore_total);

		display_core_group(&pcore_list)?;
	}

	// Display E-cores if present
	if !ecore_list.is_empty() {
		print!("\x1B[2K");
		println!("Efficiency Cores: {:6.2} W", ecore_total);

		display_core_group(&ecore_list)?;
	}

	// Display unknown cores if present and we don't have both P and E cores
	if !unknown_core_list.is_empty() && (pcore_list.is_empty() || ecore_list.is_empty()) {
		print!("\x1B[2K");
		println!("Cores: {:6.2} W", unknown_total);

		display_core_group(&unknown_core_list)?;
	}

	io::stdout().flush()
}

fn display_core_group(core_list: &[(usize, f64)]) -> io::Result<()> {
	for i in (0..core_list.len()).step_by(2) {
		let (core_id, core_power) = core_list[i];

		let core2_str = if i + 1 < core_list.len() {
			let (core2_id, core2_power) = core_list[i + 1];
			format!("| Core {}:  {:5.2} W", core2_id, core2_power)
		} else {
			String::new()
		};

		print!("\x1B[2K");
		println!("Core {}:   {:5.2} W {}", core_id, core_power, core2_str);
	}

	Ok(())
}

fn monitor_cpu_power(topology: &CpuTopology) -> io::Result<()> {
	// Show detected core types
	let core_counts = topology.count_core_types();

	println!("Monitoring CPU Power Usage (Watts) every {DATA_COLLECTION_INTERVAL_MS} ms...");

	// Show core type breakdown
	if let Some(&pcount) = core_counts.get(&CoreType::PCore) {
		print!("Performance Cores: {}", pcount);
		if let Some(&ecount) = core_counts.get(&CoreType::ECore) {
			println!(", Efficiency Cores: {}", ecount);
		} else {
			println!();
		}
	} else if let Some(&ecount) = core_counts.get(&CoreType::ECore) {
		println!("Efficiency Cores: {}", ecount);
	} else if let Some(&ucount) = core_counts.get(&CoreType::Unknown) {
		println!("Cores: {}", ucount);
	}

	println!("Press Ctrl+C to stop.");
	println!();

	let energy_unit = topology.get_energy_unit()?;
	let mut monitor = PowerMonitor::new(topology.clone());

	// Calculate how many core rows will be displayed - now accounting for separate P/E core sections
	let pcore_count = core_counts.get(&CoreType::PCore).copied().unwrap_or(0);
	let ecore_count = core_counts.get(&CoreType::ECore).copied().unwrap_or(0);
	let unknown_count = core_counts.get(&CoreType::Unknown).copied().unwrap_or(0);

	let pcore_rows = (pcore_count + 1) / 2;
	let ecore_rows = (ecore_count + 1) / 2;
	let unknown_rows = (unknown_count + 1) / 2;

	// Add headers for each section if present
	let header_count = (if pcore_count > 0 { 1 } else { 0 })
		+ (if ecore_count > 0 { 1 } else { 0 })
		+ (if unknown_count > 0 && (pcore_count == 0 || ecore_count == 0) {
			1
		} else {
			0
		});

	let total_lines = 1 + pcore_rows + ecore_rows + unknown_rows + header_count;

	for _ in 0..total_lines {
		println!();
	}

	loop {
		let initial_snapshot = topology.read_energy_snapshot()?;
		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));
		let final_snapshot = topology.read_energy_snapshot()?;

		let pkg_power = calculate_power_uw(
			initial_snapshot.package,
			final_snapshot.package,
			DATA_COLLECTION_INTERVAL_MS,
			energy_unit,
		);

		let mut core_powers = HashMap::new();

		if initial_snapshot.estimated {
			// For Intel: estimate core powers based on utilization and package power
			if let Some(ref mut intel_mapper) = monitor.intel_mapper {
				core_powers = topology.estimate_core_powers(intel_mapper, pkg_power);
			}
		} else {
			// For AMD: Calculate power for each core that has readings in both snapshots
			for core_id in initial_snapshot.cores.keys() {
				if let (Some(&start), Some(&end)) =
					(initial_snapshot.cores.get(core_id), final_snapshot.cores.get(core_id))
				{
					let power = calculate_power_uw(start, end, DATA_COLLECTION_INTERVAL_MS, energy_unit);
					core_powers.insert(*core_id, power);
				}
			}
		}

		monitor.update_readings(pkg_power, &core_powers);

		if monitor.should_update_display() {
			let readings = monitor.calculate_averages();
			display_power_readings(&readings, topology)?;
			monitor.last_display_time = Instant::now();
		}
	}
}

fn main() -> io::Result<()> {
	let topology = CpuTopology::new()?;

	println!("{:?} CPU detected.", topology.cpu_type);

	// Display detected core types
	let core_types = topology.count_core_types();
	if core_types.len() > 1 || core_types.contains_key(&CoreType::PCore) || core_types.contains_key(&CoreType::ECore) {
		println!("Hybrid architecture detected!");

		if let Some(&count) = core_types.get(&CoreType::PCore) {
			println!("  Performance cores: {}", count);
		}

		if let Some(&count) = core_types.get(&CoreType::ECore) {
			println!("  Efficiency cores: {}", count);
		}

		if let Some(&count) = core_types.get(&CoreType::Unknown) {
			println!("  Unidentified cores: {}", count);
		}
	}

	if topology.cpu_type == CpuType::Intel {
		if core_types.contains_key(&CoreType::PCore) || core_types.contains_key(&CoreType::ECore) {
			println!("Note: Using separate calibration for P-cores and E-cores.");
		} else {
			println!("Note: Using estimation for per-core power values on Intel CPUs.");
		}
		println!("Running initial calibration to measure idle power...");
	}

	monitor_cpu_power(&topology)
}
