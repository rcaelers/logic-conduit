//! SPI Register Dump
//!
//! Reads an SPI dump CSV (from a logic analyzer) and outputs the register
//! state just before the main (longest) capture begins.
//!
//! Input CSV format (as exported from logic analyzer):
//!   Id,Time[ns],1:SPI: MOSI data
//!   1,390463540.00,F88C2F
//!   2,390904260.00,78802F
//!   ...
//!
//! SPI protocol: 24-bit commands where:
//!   - Bit 23: R/W (1=read, 0=write)
//!   - Bits 22-16: Register address (7 bits, 0-127)
//!   - Bits 15-0: Register value (16 bits)
//!
//! Usage:
//!   cargo run --release --example spi_registers -- \
//!       --file _captures/vuescan-1600dpi.csv \
//!       --enable-cmd 0x600081 --disable-cmd 0x600000

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Dump SPI register state from logic analyzer CSV"
)]
struct Args {
    /// Path to SPI dump CSV file
    #[arg(short, long)]
    file: PathBuf,

    /// SPI command that enables capture (hex)
    #[arg(long, value_parser = parse_hex)]
    enable_cmd: u64,

    /// SPI command that disables capture (hex)
    #[arg(long, value_parser = parse_hex)]
    disable_cmd: u64,

    /// Output CSV file (default: derived from input filename)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Dump registers before Nth enable (auto-detect longest capture if omitted)
    #[arg(long)]
    capture_num: Option<usize>,
}

fn parse_hex(s: &str) -> Result<u64, std::num::ParseIntError> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(s, 16)
}

/// A parsed SPI command from the CSV
struct SpiCommand {
    time_ns: f64,
    mosi: u64,
}

/// Snapshot of register state at a point in time
struct RegisterSnapshot {
    enable_num: usize,
    time_ns: f64,
    registers: BTreeMap<u8, u16>,
}

fn parse_csv(path: &PathBuf) -> Result<Vec<SpiCommand>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let mut commands = Vec::new();

    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 3 {
            continue;
        }
        let time_ns: f64 = fields[1].trim().parse()?;
        let mosi = u64::from_str_radix(fields[2].trim(), 16)?;
        commands.push(SpiCommand { time_ns, mosi });
    }

    Ok(commands)
}

fn write_register_csv(path: &PathBuf, snapshot: &RegisterSnapshot) -> Result<(), std::io::Error> {
    let file = std::fs::File::create(path)?;
    let mut w = BufWriter::new(file);

    writeln!(
        w,
        "# Register snapshot before enable #{} at t={:.6}s ({} registers)",
        snapshot.enable_num,
        snapshot.time_ns / 1_000_000_000.0,
        snapshot.registers.len()
    )?;
    writeln!(w, "register,value_hex,value_dec")?;
    for (&reg, &val) in &snapshot.registers {
        writeln!(w, "0x{:02X},0x{:04X},{}", reg, val, val)?;
    }
    w.flush()?;

    println!(
        "Wrote {} registers to {}",
        snapshot.registers.len(),
        path.display()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("=== SPI Register Dump ===");
    println!("File: {}", args.file.display());
    println!("Enable command: 0x{:06X}", args.enable_cmd);
    println!("Disable command: 0x{:06X}", args.disable_cmd);

    // Derive output path from input filename if not specified
    let output_path = args.output.unwrap_or_else(|| {
        let stem = args.file.file_stem().unwrap_or_default().to_string_lossy();
        args.file.with_file_name(format!("{}_spi.csv", stem))
    });

    // Parse input CSV
    let commands = parse_csv(&args.file)?;
    println!("Parsed {} SPI commands", commands.len());

    // Process all commands: track registers, snapshots, and capture durations
    let mut registers: BTreeMap<u8, u16> = BTreeMap::new();
    let mut snapshots: Vec<RegisterSnapshot> = Vec::new();
    let mut capture_durations: Vec<(usize, f64)> = Vec::new();
    let mut enable_count: usize = 0;
    let mut last_enable_time: Option<f64> = None;

    for cmd in &commands {
        if cmd.mosi == args.enable_cmd {
            enable_count += 1;
            last_enable_time = Some(cmd.time_ns);

            snapshots.push(RegisterSnapshot {
                enable_num: enable_count,
                time_ns: cmd.time_ns,
                registers: registers.clone(),
            });

            println!(
                "  Enable #{} at t={:.3}s, {} registers set",
                enable_count,
                cmd.time_ns / 1_000_000_000.0,
                registers.len()
            );
        } else if cmd.mosi == args.disable_cmd {
            if let Some(enable_time) = last_enable_time.take() {
                let duration_ns = cmd.time_ns - enable_time;
                capture_durations.push((enable_count, duration_ns));
            }
        } else {
            // Regular register command: bit 23 = R/W, bits 22-16 = reg, bits 15-0 = value
            let is_read = (cmd.mosi >> 23) & 1 == 1;
            if !is_read {
                let register = ((cmd.mosi >> 16) & 0x7F) as u8;
                let value = (cmd.mosi & 0xFFFF) as u16;
                registers.insert(register, value);
            }
        }
    }

    // Print capture durations
    if !capture_durations.is_empty() {
        println!("\nCapture durations:");
        for (num, dur_ns) in &capture_durations {
            println!("  #{}: {:.3}s", num, dur_ns / 1_000_000_000.0);
        }
    }

    // Select the right snapshot
    let snapshot = if let Some(target) = args.capture_num {
        snapshots
            .iter()
            .find(|s| s.enable_num == target)
            .ok_or_else(|| format!("Capture #{} not found (max: {})", target, enable_count))?
    } else {
        // Auto-detect: longest capture
        let (best_num, best_dur) = capture_durations
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or("No enable/disable pairs found")?;
        println!(
            "\nLongest capture: #{} ({:.3}s)",
            best_num,
            best_dur / 1_000_000_000.0
        );
        snapshots
            .iter()
            .find(|s| s.enable_num == *best_num)
            .ok_or("Snapshot not found for longest capture")?
    };

    write_register_csv(&output_path, snapshot)?;

    Ok(())
}
