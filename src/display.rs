use std::io::{self, Write};

use crate::cpu_type::CoreType;
use crate::power::PowerReading;
use crate::topology::CpuTopology;

/// Displays power readings in the terminal with ANSI formatting
pub fn display_power_readings(readings: &PowerReading, topology: &CpuTopology) -> io::Result<()> {
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
	print!("\x1B[2K"); // Clear line
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

/// Helper function to display a group of cores in a two-column layout
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

/// Print empty lines to prepare for display
pub fn prepare_display_area(topology: &CpuTopology) -> io::Result<()> {
	// Calculate how many core rows will be displayed - now accounting for separate P/E core sections
	let core_counts = topology.count_core_types();

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

	Ok(())
}
