use msru::{Accessor, Msr};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};
use std::{fs, thread};

const ENERGY_POWER_UNIT_MSR_ADDRESS: u32 = 0xC001_0299; // MSR address for energy power unit
const ENERGY_CORE_MSR_ADDRESS: u32 = 0xC001_029A; // MSR address for core energy
const ENERGY_PACKAGE_MSR_ADDRESS: u32 = 0xC001_029B; // MSR address for package energy

const DATA_COLLECTION_INTERVAL_MS: u64 = 100; // Interval for data collection in milliseconds
const DISPLAY_UPDATE_INTERVAL_MS: u64 = 200; // Interval for updating the display in milliseconds
const AVERAGING_ITERATIONS: usize = 10; // Number of iterations for averaging power values

const POWER_SCALE: u64 = 1_000_000;

#[derive(Debug)]
enum CpuType {
	Intel,
	Amd,
	Unsupported,
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

fn read_energy_uj() -> io::Result<u64> {
	let energy_value_str = fs::read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj")?;
	energy_value_str
		.trim()
		.parse()
		.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid energy value"))
}

fn monitor_intel_rapl() -> io::Result<()> {
	println!("Monitoring CPU Power Usage using RAPL (Watts) every {DATA_COLLECTION_INTERVAL_MS} ms...");
	println!("Press Ctrl+C to stop.");

	let mut power_readings = VecDeque::with_capacity(AVERAGING_ITERATIONS);
	let mut last_display_time = Instant::now();

	loop {
		let initial_energy_uj = read_energy_uj()?;
		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));
		let final_energy_uj = read_energy_uj()?;

		let power_uw = calculate_power_uw(initial_energy_uj, final_energy_uj, DATA_COLLECTION_INTERVAL_MS, 0);
		power_readings.push_back(power_uw);

		if power_readings.len() > AVERAGING_ITERATIONS {
			power_readings.pop_front();
		}

		if last_display_time.elapsed().as_millis() >= u128::from(DISPLAY_UPDATE_INTERVAL_MS) {
			let total_power: u64 = power_readings.iter().sum();
			let average_power_uw = total_power / power_readings.len() as u64;
			let average_power_w = average_power_uw as f64 / POWER_SCALE as f64;
			print!("\rAverage Package Power: {average_power_w:.2} W");
			let _ = io::stdout().flush();
			last_display_time = Instant::now();
		}
	}
}

fn monitor_amd_msr() -> io::Result<()> {
	println!("Monitoring Package, Core, and IO Die Power Usage (Watts) every {DATA_COLLECTION_INTERVAL_MS} ms...");
	println!("Press Ctrl+C to stop.");
	println!();

	let rapl_units = read_msr(ENERGY_POWER_UNIT_MSR_ADDRESS, 0)?;
	let energy_unit = (rapl_units >> 8) & 0x1F;

	let mut power_readings = VecDeque::with_capacity(AVERAGING_ITERATIONS);
	let mut last_display_time = Instant::now();

	let total_threads = num_cpus::get();
	let physical_cores = total_threads / 2;

	let mut core_power_readings: Vec<VecDeque<u64>> =
		vec![VecDeque::with_capacity(AVERAGING_ITERATIONS); physical_cores];

	let total_lines = (physical_cores + 1) / 2 + 2;
	for _ in 0..total_lines {
		println!();
	}

	loop {
		let initial_pkg_energy = read_msr(ENERGY_PACKAGE_MSR_ADDRESS, 0)?;
		let initial_core_energy: Vec<u64> = (0..physical_cores)
			.map(|core_id| read_msr(ENERGY_CORE_MSR_ADDRESS, core_id))
			.collect::<Result<_, _>>()?;

		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));

		let final_pkg_energy = read_msr(ENERGY_PACKAGE_MSR_ADDRESS, 0)?;
		let final_core_energy: Vec<u64> = (0..physical_cores)
			.map(|core_id| read_msr(ENERGY_CORE_MSR_ADDRESS, core_id))
			.collect::<Result<_, _>>()?;

		let core_powers: Vec<u64> = initial_core_energy
			.iter()
			.zip(final_core_energy.iter())
			.map(|(&e1, &e2)| calculate_power_uw(e1, e2, DATA_COLLECTION_INTERVAL_MS, energy_unit))
			.collect();

		for (core_id, &power) in core_powers.iter().enumerate() {
			core_power_readings[core_id].push_back(power);
			if core_power_readings[core_id].len() > AVERAGING_ITERATIONS {
				core_power_readings[core_id].pop_front();
			}
		}

		let total_core_power: u64 = core_powers.iter().sum();
		let pkg_power = calculate_power_uw(
			initial_pkg_energy,
			final_pkg_energy,
			DATA_COLLECTION_INTERVAL_MS,
			energy_unit,
		);

		power_readings.push_back(pkg_power);

		if power_readings.len() > AVERAGING_ITERATIONS {
			power_readings.pop_front();
		}

		if last_display_time.elapsed().as_millis() >= u128::from(DISPLAY_UPDATE_INTERVAL_MS) {
			print!("\x1B[{}A", total_lines);

			let total_power: u64 = power_readings.iter().sum();
			let average_power_uw = total_power / power_readings.len() as u64;
			let average_power_w = average_power_uw as f64 / POWER_SCALE as f64;
			let total_core_power_w = total_core_power as f64 / POWER_SCALE as f64;

			let io_die_power_w = if average_power_w >= total_core_power_w {
				average_power_w - total_core_power_w
			} else {
				0.0
			};

			print!("\x1B[2K");
			println!(
				"Package: {average_power_w:6.2} W | IO Die: {io_die_power_w:6.2} W | Cores: {total_core_power_w:6.2} W "
			);

			print!("\x1B[2K");
			println!();

			for pair in (0..physical_cores).step_by(2) {
				let core1_avg = core_power_readings[pair].iter().sum::<u64>() as f64
					/ core_power_readings[pair].len() as f64
					/ POWER_SCALE as f64;

				let core2_str = if pair + 1 < physical_cores {
					let core2_avg = core_power_readings[pair + 1].iter().sum::<u64>() as f64
						/ core_power_readings[pair + 1].len() as f64
						/ POWER_SCALE as f64;
					format!("| Core {}:  {:5.2} W", pair + 1, core2_avg)
				} else {
					String::new()
				};

				print!("\x1B[2K");
				println!("Core {}:   {:5.2} W {}", pair, core1_avg, core2_str);
			}

			let _ = io::stdout().flush();
			last_display_time = Instant::now();
		}
	}
}

fn main() -> io::Result<()> {
	match detect_cpu_type() {
		CpuType::Intel => {
			println!("Intel CPU detected.");
			if fs::metadata("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj").is_ok() {
				monitor_intel_rapl()?;
			} else {
				eprintln!("Intel RAPL not supported on this system.");
				std::process::exit(1);
			}
		},
		CpuType::Amd => {
			println!("AMD CPU detected. Using MSR for power monitoring.");
			monitor_amd_msr()?;
		},
		CpuType::Unsupported => {
			eprintln!("Unsupported CPU type or unable to detect CPU type.");
			std::process::exit(1);
		},
	}

	Ok(())
}
