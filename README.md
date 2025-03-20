# CPU Power Monitor

A Rust tool for monitoring per-core CPU power consumption with support for hybrid architectures like Intel P-cores and E-cores.

## Features

- Real-time monitoring of CPU power consumption
- Per-core power measurements (directly measured on AMD, estimated on Intel)
- Support for Intel and AMD CPUs
- Detection and differentiated monitoring of hybrid core architectures (P-cores & E-cores)
- Terminal-based UI with color-coded display

## Requirements

- Linux operating system
- Root access (required for reading MSRs)
- Rust and Cargo installed

## Building

```bash
cargo build --release
```

## Usage

The tool must be run with root privileges to access Model-Specific Registers (MSRs).

```bash
sudo ./target/release/cpu_power_monitor
```

## How It Works

The tool uses MSR (Model-Specific Registers) to read the CPU's RAPL (Running Average Power Limit) counters. These counters provide information about energy consumption, which can be converted to power readings.

For Intel CPUs:

- Package (total CPU) power is directly measured
- Per-core power is estimated based on utilization and core type
- Hybrid architecture (P-cores and E-cores) is detected and calibrated separately

For AMD CPUs:

- Both package and per-core power values are directly measured from MSRs
- Core type detection is available but most AMD CPUs don't have hybrid architectures yet

## Project Structure

The project is organized into modules:

- `constants.rs` - Defines MSR addresses and other constants
- `cpu_type.rs` - CPU and core type detection
- `display.rs` - Terminal display formatting
- `energy.rs` - Energy reading structures
- `power.rs` - Power measurement structures
- `topology.rs` - CPU topology detection and management
- `util/` - Utility functions (MSR access, CPU utilization)
- `mapper/` - Implementations for different CPU architectures
- `monitor.rs` - Power monitoring main logic

## License

This project is licensed under the GPL v3 License - see the LICENSE file for details.
