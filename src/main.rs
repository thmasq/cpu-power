use cpu_power::cpu_type::{CoreType, CpuType};
use cpu_power::topology::CpuTopology;
use std::io;

fn main() -> io::Result<()> {
	let topology = CpuTopology::new()?;

	println!("{:?} CPU detected.", topology.cpu_type);

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

	cpu_power::monitor_cpu_power(&topology)
}
