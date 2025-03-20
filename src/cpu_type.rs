use std::fs;

use crate::constants::INTEL_CORE_TYPE_MSR;
use crate::util::msr::read_msr;

/// Represents CPU manufacturer types that can be detected
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CpuType {
	/// Intel CPU architecture
	Intel,
	/// AMD CPU architecture
	Amd,
	/// Any other CPU architecture not explicitly supported
	Unsupported,
}

/// Represents the type of CPU core in hybrid architectures
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreType {
	/// Performance core (P-core) - optimized for high performance
	PCore,
	/// Efficiency core (E-core) - optimized for power efficiency
	ECore,
	/// Unknown or standard core type (non-hybrid architecture)
	Unknown,
}

impl CoreType {
	/// Returns a human-readable string representation of the core type
	pub fn as_str(&self) -> &'static str {
		match self {
			CoreType::PCore => "P-core",
			CoreType::ECore => "E-core",
			CoreType::Unknown => "Core",
		}
	}
}

/// Detects the CPU manufacturer by reading /proc/cpuinfo
pub fn detect_cpu_type() -> CpuType {
	let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
	if cpuinfo.contains("GenuineIntel") {
		CpuType::Intel
	} else if cpuinfo.contains("AuthenticAMD") {
		CpuType::Amd
	} else {
		CpuType::Unsupported
	}
}

/// Detects core type (P-core or E-core) for a given CPU ID
///
/// This is primarily for Intel hybrid architecture CPUs (Alder Lake and newer)
/// where bit 24 in MSR 0x19A indicates the core type.
pub fn detect_core_type(cpu_id: usize) -> CoreType {
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
