// Licensed under the Apache-2.0 license

//! Test ROM build functions.
//!
//! This module provides functions for building MCU and Caliptra test ROMs,
//! including feature-specific ROM variants.

use anyhow::Result;
use std::path::PathBuf;

use crate::firmware;
use crate::CaliptraBuildArgs;
use crate::TARGET;

cfg_if::cfg_if! {
    if #[cfg(feature = "parallel-build")] {
        use rayon::prelude::*;
        fn maybe_into_par_iter<T: Send + 'static>(v: Vec<T>) -> impl ParallelIterator<Item = T> { v.into_par_iter() }
        fn maybe_par_iter<'a, T: Sync + 'a>(slice: &'a [T]) -> impl ParallelIterator<Item = &'a T> { slice.par_iter() }
    } else {
        fn maybe_into_par_iter<T: 'static>(v: Vec<T>) -> impl Iterator<Item = T> { v.into_iter() }
        fn maybe_par_iter<'a, T: 'a>(slice: &'a [T]) -> impl Iterator<Item = &'a T> { slice.iter() }
    }
}

/// Build all registered MCU test ROMs.
///
/// Iterates over `firmware::REGISTERED_FW` and builds a test ROM for each.
/// Supports parallel builds when the `parallel-build` feature is enabled.
///
/// # Arguments
///
/// - `platform`: Target platform (e.g., "emulator", "fpga").
///
/// # Returns
///
/// - `Ok(Vec<(PathBuf, String)>)`: List of (path, filename) pairs for built ROMs.
/// - `Err(anyhow::Error)`: If any ROM build fails.
pub fn build_mcu_test_roms(platform: &str) -> Result<Vec<(PathBuf, String)>> {
    let test_roms: Result<Vec<(PathBuf, String)>> =
        maybe_into_par_iter(firmware::REGISTERED_FW.to_vec())
            .map(|fwid| {
                let target_dir = if cfg!(feature = "parallel-build") {
                    Some(
                        crate::target_dir()
                            .join(format!("target-rom-{}-{}", fwid.crate_name, fwid.bin_name)),
                    )
                } else {
                    None
                };
                let bin_path = PathBuf::from(crate::test_rom_build(&CaliptraBuildArgs {
                    platform: Some(platform),
                    fwid: Some(fwid),
                    target_dir,
                    ..Default::default()
                })?);
                let filename = bin_path.file_name().unwrap().to_str().unwrap().to_string();
                Ok((bin_path, filename))
            })
            .collect();

    test_roms
}

/// Build all registered Caliptra test ROMs.
///
/// Iterates over `firmware::CPTRA_REGISTERED_FW` and builds a test ROM for each.
/// Supports parallel builds when the `parallel-build` feature is enabled.
///
/// # Arguments
///
/// - `_platform`: Target platform (currently unused, for consistency).
///
/// # Returns
///
/// - `Ok(Vec<(PathBuf, String)>)`: List of (path, filename) pairs for built ROMs.
/// - `Err(anyhow::Error)`: If any ROM build fails.
pub fn build_caliptra_test_roms(_platform: &str) -> Result<Vec<(PathBuf, String)>> {
    let cptra_test_roms: Result<Vec<(PathBuf, String)>> =
        maybe_into_par_iter(firmware::CPTRA_REGISTERED_FW.to_vec())
            .map(|fwid| {
                let filename = format!("cptra-test-rom-{}-{}.bin", fwid.crate_name, fwid.bin_name);
                let target_dir = if cfg!(feature = "parallel-build") {
                    crate::target_dir().join(format!("target-cptra-rom-{}", filename))
                } else {
                    crate::target_dir()
                };
                let release_dir = target_dir.join(TARGET).join("release");

                std::fs::create_dir_all(&release_dir)?;
                let bin_path = release_dir.join(&filename);
                let rom_bytes = caliptra_builder::build_firmware_rom(fwid)?;
                std::fs::write(&bin_path, rom_bytes)?;
                Ok((bin_path, filename))
            })
            .collect();

    cptra_test_roms
}

/// Build all test ROMs (MCU + Caliptra).
///
/// Combines the results of `build_mcu_test_roms` and `build_caliptra_test_roms`.
///
/// # Arguments
///
/// - `platform`: Target platform (e.g., "emulator", "fpga").
///
/// # Returns
///
/// - `Ok(Vec<(PathBuf, String)>)`: Combined list of all test ROM (path, filename) pairs.
/// - `Err(anyhow::Error)`: If any ROM build fails.
pub fn build_all_test_roms(platform: &str) -> Result<Vec<(PathBuf, String)>> {
    let mut test_roms = build_mcu_test_roms(platform)?;
    test_roms.extend(build_caliptra_test_roms(platform)?);
    Ok(test_roms)
}

/// Build the network ROM variants.
///
/// Builds one network ROM per feature in `network_rom_features`, distributing the
/// features over the shards. The generic network ROM is built on shard 0 whenever
/// `network_rom_features` is set.
///
/// # Arguments
///
/// - `network_rom_features`: Comma-separated list of network ROM features.
/// - `shard_index`: Index of the shard being built.
/// - `total_shards`: Total number of shards (at least 1).
///
/// # Returns
///
/// - `Ok(Vec<(PathBuf, String)>)`: List of (path, filename) pairs for built network ROMs.
/// - `Err(anyhow::Error)`: If any network ROM build fails.
pub fn build_network_roms(
    network_rom_features: Option<&str>,
    shard_index: usize,
    total_shards: usize,
) -> Result<Vec<(PathBuf, String)>> {
    let mut network_roms = network_rom_features
        .unwrap_or_default()
        .split(',')
        .filter(|feature| !feature.is_empty())
        .enumerate()
        .filter(|(idx, _)| idx % total_shards == shard_index)
        .map(|(_, feature)| {
            let path = PathBuf::from(crate::network_rom_build(Some(feature))?);
            let name = format!("network-rom-feature-{feature}.bin");
            Ok((path, name))
        })
        .collect::<Result<Vec<_>>>()?;

    if network_rom_features.is_some() && shard_index == 0 {
        network_roms.push((
            PathBuf::from(crate::network_rom_build(None)?),
            "network-rom.bin".to_string(),
        ));
    }

    Ok(network_roms)
}

/// Build feature-specific MCU ROMs.
///
/// Attempts to build a ROM with each feature flag. If a feature is not
/// supported by the ROM crate, it is skipped (tests will fall back to
/// the generic MCU ROM).
///
/// # Arguments
///
/// - `platform`: Target platform (e.g., "emulator", "fpga").
/// - `features`: List of feature flags to build ROMs for.
///
/// # Returns
///
/// - `Ok(Vec<(PathBuf, String)>)`: List of successfully built (path, filename) pairs.
/// - `Err(anyhow::Error)`: If a critical error occurs (not feature-not-supported).
pub fn build_feature_roms(platform: &str, features: &[&str]) -> Result<Vec<(PathBuf, String)>> {
    let feature_roms: Result<Vec<(PathBuf, String)>> = maybe_par_iter(features)
        .filter_map(|feature| {
            let target_dir = if cfg!(feature = "parallel-build") {
                Some(crate::target_dir().join(format!("target-feature-rom-{}", feature)))
            } else {
                None
            };
            match crate::rom_build(&CaliptraBuildArgs {
                platform: Some(platform),
                features: Some(feature),
                target_dir,
                ..Default::default()
            }) {
                Ok(rom_path) => {
                    let rom_name = format!("mcu-test-rom-feature-{}.bin", feature);
                    println!("Built feature ROM: {rom_path:?} -> {}", rom_name);
                    Some(Ok((rom_path, rom_name)))
                }
                Err(e) => {
                    println!(
                        "Skipping feature ROM for {}: {} (will use generic ROM)",
                        feature, e
                    );
                    None
                }
            }
        })
        .collect();

    feature_roms
}
