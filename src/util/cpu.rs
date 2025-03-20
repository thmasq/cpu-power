use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};

use crate::cpu_type::CoreType;

/// Statistics for a single CPU
#[derive(Debug, Clone, Copy)]
pub struct CpuStats {
	pub user: u64,
	pub nice: u64,
	pub system: u64,
	pub idle: u64,
	pub iowait: u64,
	pub irq: u64,
	pub softirq: u64,
	pub steal: u64,
	pub total: u64,
}

/// Tracks CPU utilization across all cores/threads
#[derive(Debug, Clone)]
pub struct CpuUtilization {
	prev_stats: HashMap<usize, CpuStats>,
	utilization: HashMap<usize, f64>,
}

impl CpuUtilization {
	/// Creates a new CpuUtilization tracker
	pub fn new() -> Self {
		Self {
			prev_stats: HashMap::new(),
			utilization: HashMap::new(),
		}
	}

	/// Updates CPU utilization statistics by reading /proc/stat
	pub fn update(&mut self) -> io::Result<()> {
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

	/// Calculates per-core utilization by averaging the utilization of all threads in each core
	pub fn get_core_utilization(&self, thread_to_core: &HashMap<usize, (usize, CoreType)>) -> HashMap<usize, f64> {
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
