use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use std::{io, thread};

use crate::constants::{AVERAGING_ITERATIONS, DATA_COLLECTION_INTERVAL_MS, DISPLAY_UPDATE_INTERVAL_MS, POWER_SCALE};
use crate::mapper::intel::IntelCoreMapper;
use crate::power::PowerReading;
use crate::topology::CpuTopology;
use crate::util::calculate_power_uw;

/// Monitors and records CPU power consumption
pub struct PowerMonitor {
	pub topology: CpuTopology,
	power_readings: VecDeque<u64>,
	core_power_readings: HashMap<usize, VecDeque<u64>>, // Physical core ID -> readings
	pub last_display_time: Instant,
	pub intel_mapper: Option<IntelCoreMapper>, // Only used for Intel estimation
	estimated: bool,
}

impl PowerMonitor {
	/// Creates a new PowerMonitor for the given CPU topology
	pub fn new(topology: CpuTopology) -> Self {
		let mut core_power_readings = HashMap::new();
		for &core_id in topology.core_to_threads.keys() {
			core_power_readings.insert(core_id, VecDeque::with_capacity(AVERAGING_ITERATIONS));
		}

		// Create Intel mapper for estimation if needed
		let mut intel_mapper = if topology.cpu_type == crate::cpu_type::CpuType::Intel {
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

		let estimated = topology.cpu_type == crate::cpu_type::CpuType::Intel;

		Self {
			topology,
			power_readings: VecDeque::with_capacity(AVERAGING_ITERATIONS),
			core_power_readings,
			last_display_time: Instant::now(),
			intel_mapper,
			estimated,
		}
	}

	/// Updates the internal power readings with new measurements
	pub fn update_readings(&mut self, package_power: u64, core_powers: &HashMap<usize, u64>) {
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

	/// Calculates average power readings from stored values
	pub fn calculate_averages(&self) -> PowerReading {
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

	/// Calculates the average power from a deque of readings
	fn calculate_average_power(&self, readings: &VecDeque<u64>) -> f64 {
		if readings.is_empty() {
			return 0.0;
		}
		let total: u64 = readings.iter().sum();
		total as f64 / readings.len() as f64 / POWER_SCALE as f64
	}

	/// Checks if it's time to update the display
	pub fn should_update_display(&self) -> bool {
		self.last_display_time.elapsed().as_millis() >= u128::from(DISPLAY_UPDATE_INTERVAL_MS)
	}

	/// Monitors CPU power continuously, updating readings and displaying results
	pub fn monitor_cpu_power(&mut self) -> io::Result<()> {
		let energy_unit = self.topology.get_energy_unit()?;

		loop {
			let initial_snapshot = self.topology.read_energy_snapshot()?;
			thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));
			let final_snapshot = self.topology.read_energy_snapshot()?;

			let pkg_power = calculate_power_uw(
				initial_snapshot.package,
				final_snapshot.package,
				DATA_COLLECTION_INTERVAL_MS,
				energy_unit,
			);

			let mut core_powers = HashMap::new();

			if initial_snapshot.estimated {
				// For Intel: estimate core powers based on utilization and package power
				if let Some(ref mut intel_mapper) = self.intel_mapper {
					core_powers = self.topology.estimate_core_powers(intel_mapper, pkg_power);
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

			self.update_readings(pkg_power, &core_powers);

			yield_hook(); // Allow caller to act on updated readings
		}
	}
}

// This is a hook for testing or potential future integration
// If we ever need to inject behavior during the monitoring loop
#[inline]
fn yield_hook() {}
