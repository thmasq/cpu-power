pub mod constants;
pub mod cpu_type;
pub mod display;
pub mod energy;
pub mod mapper;
pub mod monitor;
pub mod power;
pub mod topology;
pub mod util;

use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::{io, thread};

use crate::display::{display_power_readings, prepare_display_area};
use crate::monitor::PowerMonitor;
use crate::topology::CpuTopology;

/// Starts monitoring CPU power and displays the results
///
/// This is the main entry point for the power monitoring functionality.
pub fn monitor_cpu_power(topology: &CpuTopology) -> io::Result<()> {
	// Show detected core types
	let core_counts = topology.count_core_types();

	println!(
		"Monitoring CPU Power Usage (Watts) every {} ms...",
		constants::DATA_COLLECTION_INTERVAL_MS
	);

	// Show core type breakdown
	if let Some(&pcount) = core_counts.get(&cpu_type::CoreType::PCore) {
		print!("Performance Cores: {}", pcount);
		if let Some(&ecount) = core_counts.get(&cpu_type::CoreType::ECore) {
			println!(", Efficiency Cores: {}", ecount);
		} else {
			println!();
		}
	} else if let Some(&ecount) = core_counts.get(&cpu_type::CoreType::ECore) {
		println!("Efficiency Cores: {}", ecount);
	} else if let Some(&ucount) = core_counts.get(&cpu_type::CoreType::Unknown) {
		println!("Cores: {}", ucount);
	}

	println!("Press Ctrl+C to stop.");
	println!();

	// Prepare display area
	prepare_display_area(topology)?;

	// Create a power monitor
	let mut monitor = PowerMonitor::new(topology.clone());
	let energy_unit = topology.get_energy_unit()?;

	loop {
		// Take initial snapshot
		let initial_snapshot = topology.read_energy_snapshot()?;

		// Wait for the collection interval
		thread::sleep(Duration::from_millis(constants::DATA_COLLECTION_INTERVAL_MS));

		// Take final snapshot
		let final_snapshot = topology.read_energy_snapshot()?;

		// Calculate package power
		let pkg_power = util::calculate_power_uw(
			initial_snapshot.package,
			final_snapshot.package,
			constants::DATA_COLLECTION_INTERVAL_MS,
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
					let power =
						util::calculate_power_uw(start, end, constants::DATA_COLLECTION_INTERVAL_MS, energy_unit);
					core_powers.insert(*core_id, power);
				}
			}
		}

		// Update power readings
		monitor.update_readings(pkg_power, &core_powers);

		// Check if it's time to update the display
		if monitor.should_update_display() {
			let readings = monitor.calculate_averages();
			display_power_readings(&readings, topology)?;
			monitor.last_display_time = Instant::now();
		}
	}
}
