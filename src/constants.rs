// AMD RAPL MSR addresses
pub const AMD_ENERGY_UNIT_MSR: u32 = 0xC001_0299;
pub const AMD_ENERGY_CORE_MSR: u32 = 0xC001_029A;
pub const AMD_ENERGY_PKG_MSR: u32 = 0xC001_029B;

// Intel RAPL MSR addresses
pub const INTEL_POWER_UNIT_MSR: u32 = 0x606;
pub const INTEL_PKG_ENERGY_MSR: u32 = 0x611;

// Intel Hybrid Architecture MSRs
pub const INTEL_CORE_TYPE_MSR: u32 = 0x19A;

// Monitoring and display settings
pub const DATA_COLLECTION_INTERVAL_MS: u64 = 100;
pub const DISPLAY_UPDATE_INTERVAL_MS: u64 = 200;
pub const AVERAGING_ITERATIONS: usize = 10;
pub const POWER_SCALE: u64 = 1_000_000;
