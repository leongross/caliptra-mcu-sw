// Licensed under the Apache-2.0 license

//! Flash image creation utilities.
//!
//! This module provides functions for creating flash images with optional
//! partition tables for different boot modes.

use anyhow::Result;
use caliptra_mcu_config::boot::{PartitionId, PartitionStatus, RollbackEnable};
use caliptra_mcu_config_emulator::flash::{
    PartitionTable, StandAloneChecksumCalculator, IMAGE_A_PARTITION,
};
use std::path::{Path, PathBuf};

use crate::{CaliptraBuildArgs, ImageCfg};

/// Inputs for [`create_flash_image`].
pub struct FlashImageBuildArgs<'a> {
    /// Optional path to the Caliptra firmware bundle.
    pub caliptra_fw_path: Option<PathBuf>,
    /// Optional path to the SoC manifest.
    pub soc_manifest_path: Option<PathBuf>,
    /// Optional path to the Owner Authorization manifest.
    pub owner_auth_manifest_path: Option<PathBuf>,
    /// Optional path to the Owner measurement policy.
    pub owner_measurement_policy_path: Option<PathBuf>,
    /// Owner SoC images to include.
    pub owner_soc_images: Option<Vec<ImageCfg>>,
    /// Optional path to the MCU runtime binary.
    pub mcu_runtime_path: Option<PathBuf>,
    /// Optional MCU image configuration.
    pub mcu_image_cfg: Option<ImageCfg>,
    /// File name of the Caliptra firmware in the network boot section.
    pub caliptra_firmware_network_filename: Option<&'a str>,
    /// File name of the SoC manifest in the network boot section.
    pub soc_manifest_network_filename: Option<&'a str>,
    /// Paths to additional SoC images to include.
    pub soc_images_paths: Vec<PathBuf>,
    /// SoC image configurations matching `soc_images_paths`.
    pub soc_images: Option<Vec<ImageCfg>>,
    /// If true, creates a partition table at offset 0.
    pub is_flash_based_boot: bool,
}

/// Create a flash image from firmware components.
///
/// # Arguments
///
/// - `args`: Firmware components to include. See [`FlashImageBuildArgs`].
///
/// # Returns
///
/// - `Ok(PathBuf)`: Path to the created flash image (temporary file).
/// - `Err(anyhow::Error)`: If flash image creation fails.
pub fn create_flash_image(args: FlashImageBuildArgs<'_>) -> Result<PathBuf> {
    let FlashImageBuildArgs {
        caliptra_fw_path,
        soc_manifest_path,
        owner_auth_manifest_path,
        owner_measurement_policy_path,
        owner_soc_images,
        mcu_runtime_path,
        mcu_image_cfg,
        caliptra_firmware_network_filename,
        soc_manifest_network_filename,
        soc_images_paths,
        soc_images,
        is_flash_based_boot,
    } = args;

    let flash_image_path = tempfile::NamedTempFile::new()
        .expect("Failed to create flash image file")
        .path()
        .to_path_buf();

    // For flash-based boot, we need to:
    // 1. Write flash content at the partition offset (not 0)
    // 2. Write a valid partition table at offset 0
    let flash_offset = if is_flash_based_boot {
        IMAGE_A_PARTITION.offset
    } else {
        0
    };

    crate::flash_image::flash_image_create(&CaliptraBuildArgs {
        caliptra_firmware: caliptra_fw_path,
        caliptra_firmware_network_filename: caliptra_firmware_network_filename.map(str::to_string),
        soc_manifest: soc_manifest_path,
        soc_manifest_network_filename: soc_manifest_network_filename.map(str::to_string),
        owner_auth_manifest: owner_auth_manifest_path,
        owner_measurement_policy: owner_measurement_policy_path,
        owner_soc_images,
        mcu_firmware: mcu_runtime_path,
        mcu_image_cfg,
        soc_image_paths: Some(
            soc_images_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        ),
        soc_images,
        offset: flash_offset,
        output_path: Some(flash_image_path.to_string_lossy().to_string()),
        ..Default::default()
    })?;

    // For flash-based boot, write a valid partition table at offset 0
    if is_flash_based_boot {
        write_partition_table(&flash_image_path)?;
    }

    Ok(flash_image_path)
}

/// Write a partition table to a flash image at offset 0.
///
/// # Arguments
///
/// - `flash_image_path`: Path to the flash image file.
///
/// # Returns
///
/// - `Ok(())`: If the partition table was written successfully.
/// - `Err(anyhow::Error)`: If writing fails.
fn write_partition_table(flash_image_path: &Path) -> Result<()> {
    let mut partition_table = PartitionTable {
        active_partition: PartitionId::A as u32,
        partition_a_status: PartitionStatus::Valid as u16,
        partition_b_status: PartitionStatus::Invalid as u16,
        rollback_enable: RollbackEnable::Enabled as u32,
        ..Default::default()
    };
    let checksum_calculator = StandAloneChecksumCalculator::new();
    partition_table.populate_checksum(&checksum_calculator);

    crate::flash_image::write_partition_table(
        &partition_table,
        0,
        flash_image_path.to_str().unwrap(),
    )?;

    Ok(())
}
