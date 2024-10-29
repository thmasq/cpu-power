use msru::{Accessor, Msr};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};
use std::{fs, thread};

const ENERGY_POWER_UNIT_MSR_ADDRESS: u32 = 0xC0010299; // MSR address for energy power unit
const ENERGY_CORE_MSR_ADDRESS: u32 = 0xC001029A; // MSR address for core energy
const ENERGY_PACKAGE_MSR_ADDRESS: u32 = 0xC001029B; // MSR address for package energy

const DATA_COLLECTION_INTERVAL_MS: u64 = 100; // Interval for data collection in milliseconds
const DISPLAY_UPDATE_INTERVAL_MS: u64 = 200; // Interval for updating the display in milliseconds
const AVERAGING_ITERATIONS: usize = 10; // Number of iterations for averaging power values

#[derive(Debug)]
enum CpuType {
	Intel,
	AMD,
	Unsupported,
}

fn detect_cpu_type() -> CpuType {
	let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
	if cpuinfo.contains("GenuineIntel") {
		CpuType::Intel
	} else if cpuinfo.contains("AuthenticAMD") {
		CpuType::AMD
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

fn calculate_power(energy_start: u64, energy_end: u64, time_interval_seconds: f64, energy_unit: u64) -> f64 {
	let energy_difference = if energy_end < energy_start {
		energy_end + 0xFFFFFFFF - energy_start
	} else {
		energy_end - energy_start
	};

	(energy_difference as f64 * 1_000_000.0)
		/ (2u64.pow(energy_unit as u32) as f64 * time_interval_seconds * 1_000_000.0)
}

fn read_energy_joules() -> io::Result<u64> {
	let energy_value_str = fs::read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj")?;
	energy_value_str
		.trim()
		.parse()
		.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid energy value"))
}

fn monitor_intel_rapl() -> io::Result<()> {
	println!(
		"Monitoring CPU Power Usage using RAPL (Watts) every {} ms...",
		DATA_COLLECTION_INTERVAL_MS
	);
	println!("Press Ctrl+C to stop.");

	let mut power_readings = VecDeque::with_capacity(AVERAGING_ITERATIONS);
	let mut last_display_time = Instant::now();

	loop {
		let initial_energy_joules = read_energy_joules()?;
		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));
		let final_energy_joules = read_energy_joules()?;

		let power = calculate_power(
			initial_energy_joules,
			final_energy_joules,
			DATA_COLLECTION_INTERVAL_MS as f64 / 1000.0,
			0,
		);
		power_readings.push_back(power);

		if power_readings.len() > AVERAGING_ITERATIONS {
			power_readings.pop_front();
		}

		if last_display_time.elapsed().as_millis() >= DISPLAY_UPDATE_INTERVAL_MS as u128 {
			let average_power: f64 = power_readings.iter().sum::<f64>() / power_readings.len() as f64;
			print!("\rAverage Package Power: {:.2} W", average_power);
			let _ = io::stdout().flush();
			last_display_time = Instant::now();
		}
	}
}

fn monitor_amd_msr() -> io::Result<()> {
	println!(
		"Monitoring Package and Core Power Usage (Watts) every {} ms...",
		DATA_COLLECTION_INTERVAL_MS
	);
	println!("Press Ctrl+C to stop.");

	let rapl_units = read_msr(ENERGY_POWER_UNIT_MSR_ADDRESS, 0)?;
	let energy_unit = (rapl_units >> 8) & 0x1F;

	let mut power_readings = VecDeque::with_capacity(AVERAGING_ITERATIONS);
	let mut last_display_time = Instant::now();

	let total_cores = num_cpus::get();

	loop {
		let initial_pkg_energy = read_msr(ENERGY_PACKAGE_MSR_ADDRESS, 0)?;
		let initial_core_energy: Vec<u64> = (0..total_cores)
			.map(|core_id| read_msr(ENERGY_CORE_MSR_ADDRESS, core_id))
			.collect::<Result<_, _>>()?;

		thread::sleep(Duration::from_millis(DATA_COLLECTION_INTERVAL_MS));

		let final_pkg_energy = read_msr(ENERGY_PACKAGE_MSR_ADDRESS, 0)?;
		let final_core_energy: Vec<u64> = (0..total_cores)
			.map(|core_id| read_msr(ENERGY_CORE_MSR_ADDRESS, core_id))
			.collect::<Result<_, _>>()?;

		let total_core_power: f64 = initial_core_energy
			.iter()
			.zip(final_core_energy.iter())
			.map(|(&e1, &e2)| calculate_power(e1, e2, DATA_COLLECTION_INTERVAL_MS as f64 / 1000.0, energy_unit))
			.sum();

		let pkg_power = calculate_power(
			initial_pkg_energy,
			final_pkg_energy,
			DATA_COLLECTION_INTERVAL_MS as f64 / 1000.0,
			energy_unit,
		);

		power_readings.push_back(pkg_power);

		if power_readings.len() > AVERAGING_ITERATIONS {
			power_readings.pop_front();
		}

		if last_display_time.elapsed().as_millis() >= DISPLAY_UPDATE_INTERVAL_MS as u128 {
			let average_pkg_power: f64 = power_readings.iter().sum::<f64>() / power_readings.len() as f64;
			print!(
				"\rAverage Package Power: {:6.2} W | Total Core Power: {:6.2} W",
				average_pkg_power, total_core_power
			);
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
		CpuType::AMD => {
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
