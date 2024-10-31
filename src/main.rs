use msru::{Accessor, Msr};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};
use std::{fs, thread};

// AMD RAPL MSR addresses
const AMD_ENERGY_UNIT_MSR: u32 = 0xC001_0299;
const AMD_ENERGY_CORE_MSR: u32 = 0xC001_029A;
const AMD_ENERGY_PKG_MSR: u32 = 0xC001_029B;

// Intel RAPL MSR addresses
const INTEL_POWER_UNIT_MSR: u32 = 0x606;
const INTEL_PKG_ENERGY_MSR: u32 = 0x611;
const INTEL_CORE_ENERGY_MSR: u32 = 0x639;

const DATA_COLLECTION_INTERVAL_MS: u64 = 100;
const DISPLAY_UPDATE_INTERVAL_MS: u64 = 200;
const AVERAGING_ITERATIONS: usize = 10;
const POWER_SCALE: u64 = 1_000_000;

#[derive(Debug)]
enum CpuType {
	Intel,
	Amd,
	Unsupported,
}

struct PowerReading {
	package: f64,
	cores: Vec<f64>,
}

struct EnergySnapshot {
	package: u64,
	cores: Vec<u64>,
}

struct PowerMonitor {
	power_readings: VecDeque<u64>,
	core_power_readings: Vec<VecDeque<u64>>,
	last_display_time: Instant,
}

impl PowerMonitor {
	fn new(physical_cores: usize) -> Self {
		Self {
			power_readings: VecDeque::with_capacity(AVERAGING_ITERATIONS),
			core_power_readings: vec![VecDeque::with_capacity(AVERAGING_ITERATIONS); physical_cores],
			last_display_time: Instant::now(),
		}
	}

	fn update_readings(&mut self, package_power: u64, core_powers: &[u64]) {
		self.power_readings.push_back(package_power);
		if self.power_readings.len() > AVERAGING_ITERATIONS {
			self.power_readings.pop_front();
		}

		for (core_id, &power) in core_powers.iter().enumerate() {
			self.core_power_readings[core_id].push_back(power);
			if self.core_power_readings[core_id].len() > AVERAGING_ITERATIONS {
				self.core_power_readings[core_id].pop_front();
			}
		}
	}

	fn calculate_averages(&self) -> PowerReading {
		let package_avg = self.calculate_average_power(&self.power_readings);
		let cores: Vec<f64> = self
			.core_power_readings
			.iter()
			.map(|readings| self.calculate_average_power(readings))
			.collect();

		PowerReading {
			package: package_avg,
			cores,
		}
	}

	fn calculate_average_power(&self, readings: &VecDeque<u64>) -> f64 {
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

fn read_msr(msr_address: u32, core_id: usize) -> io::Result<u64> {
	Msr::new(msr_address, core_id as u16)
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

fn display_power_readings(readings: &PowerReading, physical_cores: usize) -> io::Result<()> {
	let total_lines = (physical_cores + 1) / 2 + 2;
	print!("\x1B[{}A", total_lines);

	print!("\x1B[2K");
	println!(
		"Package: {:6.2} W | Cores Total: {:6.2} W",
		readings.package,
		readings.cores.iter().sum::<f64>()
	);

	print!("\x1B[2K");
	println!();

	for pair in (0..physical_cores).step_by(2) {
		let core2_str = if pair + 1 < physical_cores {
			format!("| Core {}:  {:5.2} W", pair + 1, readings.cores[pair + 1])
		} else {
			String::new()
		};

		print!("\x1B[2K");
		println!("Core {}:   {:5.2} W {}", pair, readings.cores[pair], core2_str);
	}

	io::stdout().flush()
}

fn read_energy_snapshot(cpu_type: &CpuType, physical_cores: usize) -> io::Result<EnergySnapshot> {
	match cpu_type {
		CpuType::Intel => {
			let package = read_msr(INTEL_PKG_ENERGY_MSR, 0)?;
			let cores = vec![read_msr(INTEL_CORE_ENERGY_MSR, 0)?];
			Ok(EnergySnapshot { package, cores })
		},
		CpuType::Amd => {
			let package = read_msr(AMD_ENERGY_PKG_MSR, 0)?;
			let cores = (0..physical_cores)
				.map(|core_id| read_msr(AMD_ENERGY_CORE_MSR, core_id))
				.collect::<Result<Vec<_>, _>>()?;
			Ok(EnergySnapshot { package, cores })
		},
		CpuType::Unsupported => Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported CPU type")),
	}
}

fn get_energy_unit(cpu_type: &CpuType) -> io::Result<u64> {
	let unit_msr = match cpu_type {
		CpuType::Intel => read_msr(INTEL_POWER_UNIT_MSR, 0)?,
		CpuType::Amd => read_msr(AMD_ENERGY_UNIT_MSR, 0)?,
		CpuType::Unsupported => return Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported CPU type")),
	};
	Ok((unit_msr >> 8) & 0x1F)
}

fn monitor_cpu_power(cpu_type: &CpuType) -> io::Result<()> {
	println!("Monitoring CPU Power Usage (Watts) every {DATA_COLLECTION_INTERVAL_MS} ms...");
	println!("Press Ctrl+C to stop.");
	println!();

	let energy_unit = get_energy_unit(cpu_type)?;
	let physical_cores = if matches!(cpu_type, CpuType::Intel) {
		1
	} else {
		num_cpus::get() / 2
	};

	let mut monitor = PowerMonitor::new(physical_cores);

	let total_lines = (physical_cores + 1) / 2 + 2;
	for _ in 0..total_lines {
		println!();
	}

	loop {
		let initial_snapshot = read_energy_snapshot(cpu_type, physical_cores)?;
		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));
		let final_snapshot = read_energy_snapshot(cpu_type, physical_cores)?;

		let pkg_power = calculate_power_uw(
			initial_snapshot.package,
			final_snapshot.package,
			DATA_COLLECTION_INTERVAL_MS,
			energy_unit,
		);

		let core_powers: Vec<u64> = initial_snapshot
			.cores
			.iter()
			.zip(final_snapshot.cores.iter())
			.map(|(&start, &end)| calculate_power_uw(start, end, DATA_COLLECTION_INTERVAL_MS, energy_unit))
			.collect();

		monitor.update_readings(pkg_power, &core_powers);

		if monitor.should_update_display() {
			let readings = monitor.calculate_averages();
			display_power_readings(&readings, physical_cores)?;
			monitor.last_display_time = Instant::now();
		}
	}
}

fn main() -> io::Result<()> {
	let cpu_type = detect_cpu_type();
	match cpu_type {
		CpuType::Intel => {
			println!("Intel CPU detected.");
			monitor_cpu_power(&cpu_type)
		},
		CpuType::Amd => {
			println!("AMD CPU detected.");
			monitor_cpu_power(&cpu_type)
		},
		CpuType::Unsupported => {
			eprintln!("Unsupported CPU type or unable to detect CPU type.");
			std::process::exit(1);
		},
	}
}
