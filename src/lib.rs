pub mod constants;
pub mod cpu_type;
pub mod display;
pub mod energy;
pub mod mapper;
pub mod monitor;
pub mod power;
pub mod topology;
pub mod util;

use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{io, thread};

use crate::display::{display_power_readings, prepare_display_area};
use crate::monitor::PowerMonitor;
use crate::power::PowerReading;
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

	// Clone the topology for both threads to avoid reference lifetime issues
	let monitoring_topology = topology.clone();
	let display_topology = topology.clone();

	// Create a channel to send power readings from the monitoring thread to the display thread
	let (tx, rx) = mpsc::channel::<PowerReading>();

	// Prepare display area
	prepare_display_area(topology)?;

	// Create and start the display thread
	let display_handle = thread::spawn(move || {
		let mut last_display_time = Instant::now();

		loop {
			if last_display_time.elapsed().as_millis() >= u128::from(constants::DISPLAY_UPDATE_INTERVAL_MS) {
				match rx.try_recv() {
					Ok(reading) => {
						if let Err(e) = display_power_readings(&reading, &display_topology) {
							eprintln!("Display error: {}", e);
							break;
						}
						last_display_time = Instant::now();
					},
					Err(mpsc::TryRecvError::Empty) => {
						// No new reading yet, just continue
					},
					Err(mpsc::TryRecvError::Disconnected) => {
						// Monitoring thread has ended
						break;
					},
				}
			}
			thread::sleep(Duration::from_millis(10));
		}
	});

	// Run the monitoring thread in the main thread
	let monitoring_result: io::Result<()> = thread::scope(|_| {
		let mut monitor = PowerMonitor::new(monitoring_topology);
		let energy_unit = monitor.topology.get_energy_unit()?;

		loop {
			let initial_snapshot = monitor.topology.read_energy_snapshot()?;
			thread::sleep(Duration::from_millis(constants::DATA_COLLECTION_INTERVAL_MS));
			let final_snapshot = monitor.topology.read_energy_snapshot()?;

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
					core_powers = monitor.topology.estimate_core_powers(intel_mapper, pkg_power);
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

			monitor.update_readings(pkg_power, &core_powers);

			// Send the latest reading to the display thread
			let _ = tx.send(monitor.calculate_averages());

			// Check if the display thread has ended (e.g., due to Ctrl+C)
			if display_handle.is_finished() {
				break;
			}
		}

		Ok(())
	});

	// Handle any errors from the monitoring thread
	if let Err(e) = monitoring_result {
		eprintln!("Monitoring error: {}", e);
	}

	// Wait for the display thread to finish
	let _ = display_handle.join();

	Ok(())
}

use std::collections::HashMap;
