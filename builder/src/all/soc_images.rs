// Licensed under the Apache-2.0 license

//! SoC image creation utilities.
//!
//! This module provides utilities for creating default SoC images
//! used in tests that require them, as well as the default MCU image
//! configurations and generated attestation configuration helpers.

use anyhow::Result;
use caliptra_mcu_flash_image::MCU_RT_IDENTIFIER;
use std::path::{Path, PathBuf};

use crate::ImageCfg;

/// MCI base address for SoC image load addresses.
/// Uses FPGA memory map since the emulator's AXI simulation uses FPGA-like addresses.
const MCI_BASE_AXI_ADDRESS: u64 = caliptra_mcu_config_fpga::FPGA_MEMORY_MAP.mci_offset as u64;

/// MCU MBOX SRAM1 offset from MCI base.
/// Matches caliptra_mcu_mbox_driver::MCU_MBOX1_SRAM_OFFSET (0x80_0000).
const MCU_MBOX_SRAM1_OFFSET: u64 = 0x80_0000;

/// MCU SRAM offset from MCI base.
const MCU_SRAM_OFFSET: u64 = 0xC0_0000;

const ATTESTATION_SOC_FW_ID: u32 = 0x2000;
const ATTESTATION_SOC_FW_ID_2: u32 = 0x2001;

/// Creates default SoC images for tests that require them.
///
/// Creates two simple test images with different patterns (0x55 and 0xAA)
/// that can be used for testing SoC image loading functionality.
///
/// # Returns
///
/// - `(Vec<ImageCfg>, Vec<PathBuf>)`: Tuple of (image configurations, image file paths).
pub fn create_default_soc_images() -> (Vec<ImageCfg>, Vec<PathBuf>) {
    let soc_image_fw_1 = vec![0x55u8; 512];
    let soc_image_fw_2 = vec![0xAAu8; 256];

    let soc_image_path_1 = std::env::temp_dir().join("default-soc-image-1.bin");
    let soc_image_path_2 = std::env::temp_dir().join("default-soc-image-2.bin");

    std::fs::write(&soc_image_path_1, &soc_image_fw_1).expect("Failed to write SoC image 1");
    std::fs::write(&soc_image_path_2, &soc_image_fw_2).expect("Failed to write SoC image 2");

    let soc_images = vec![
        ImageCfg {
            path: soc_image_path_1.clone(),
            load_addr: MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET,
            image_id: 4096,
            component_id: 4096,
            exec_bit: 5,
            ..Default::default()
        },
        ImageCfg {
            path: soc_image_path_2.clone(),
            load_addr: MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET + soc_image_fw_1.len() as u64,
            image_id: 4097,
            component_id: 4097,
            exec_bit: 6,
            ..Default::default()
        },
    ];

    let soc_images_paths = vec![soc_image_path_1, soc_image_path_2];

    (soc_images, soc_images_paths)
}

/// Creates a default Owner SoC image.
///
/// Caliptra currently rejects owner manifests with an empty metadata collection,
/// so a dummy image is placed right after the last vendor SoC image.
///
/// # Arguments
///
/// - `soc_images`: Vendor SoC images the owner image is placed after.
///
/// # Returns
///
/// - `Vec<ImageCfg>`: A single Owner SoC image configuration.
pub fn create_default_owner_soc_images(soc_images: &[ImageCfg]) -> Vec<ImageCfg> {
    let owner_soc_image_path = std::env::temp_dir().join("default-owner-soc-image.bin");
    std::fs::write(&owner_soc_image_path, vec![0xCCu8; 128])
        .expect("Failed to write owner SoC image");
    let load_addr = soc_images
        .iter()
        .map(|image| {
            image.load_addr
                + std::fs::metadata(&image.path)
                    .expect("Failed to read SoC image metadata")
                    .len()
        })
        .max()
        .unwrap_or(MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET);

    vec![ImageCfg {
        path: owner_soc_image_path.clone(),
        load_addr,
        image_id: 0x10000,
        component_id: 0x10000,
        exec_bit: 7,
        ..Default::default()
    }]
}

/// Creates dummy SoC images for attestation integration tests.
///
/// The test features intentionally leave `is_ak_target = false` so MCU Runtime
/// remains the attestation node. The TCB/non-TCB flags exercise both
/// Measurement API evidence read paths.
///
/// # Arguments
///
/// - `feature`: Attestation test feature the images are created for.
///
/// # Returns
///
/// - `(Vec<ImageCfg>, Vec<PathBuf>)`: Tuple of (image configurations, image file paths).
pub fn create_attestation_soc_images(feature: Option<&str>) -> (Vec<ImageCfg>, Vec<PathBuf>) {
    create_attestation_soc_images_variant(feature, false)
}

/// Same as [`create_attestation_soc_images`], but `post_update` selects the
/// payload shipped in the hitless update package.
///
/// The post-update images keep the same `fw_id`/`component_id`/TCB flags and
/// load addresses, and only differ in content. That makes the SoC component
/// digests in the post-update evidence differ from the cold-boot ones, so the
/// two appraisals are distinguishable instead of being byte-identical.
pub fn create_attestation_soc_images_variant(
    feature: Option<&str>,
    post_update: bool,
) -> (Vec<ImageCfg>, Vec<PathBuf>) {
    // The default OCP EAT attestation scenario uses only non-TCB SoC
    // components; dedicated features opt in to TCB-only or mixed coverage.
    let specs: &[(u8, u32, bool)] = match feature {
        Some("test-mctp-spdm-attestation-tcb") | Some("test-mctp-spdm-attestation-hitless-tcb") => {
            &[(0xde, ATTESTATION_SOC_FW_ID, true)]
        }
        Some("test-mctp-spdm-attestation-mixed")
        | Some("test-mctp-spdm-attestation-hitless-mixed") => &[
            (0xde, ATTESTATION_SOC_FW_ID, true),
            (0xad, ATTESTATION_SOC_FW_ID_2, false),
        ],
        _ => &[(0xde, ATTESTATION_SOC_FW_ID, false)],
    };

    let mut soc_images = Vec::with_capacity(specs.len());
    let mut soc_image_paths = Vec::with_capacity(specs.len());
    let mut load_addr = MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET;

    for (idx, (fill, fw_id, is_tcb)) in specs.iter().enumerate() {
        // Invert the fill byte for the update payload so the SHA-384 of the
        // component changes while its size and placement stay identical.
        let fill = if post_update { !*fill } else { *fill };
        let suffix = if post_update { "-update" } else { "" };
        let soc_image_fw = vec![fill; 512];
        let soc_image_path =
            std::env::temp_dir().join(format!("attestation-soc-image-{fw_id:08x}{suffix}.bin"));
        std::fs::write(&soc_image_path, &soc_image_fw)
            .expect("Failed to write attestation SoC image");
        soc_images.push(ImageCfg {
            path: soc_image_path.clone(),
            load_addr,
            image_id: *fw_id,
            component_id: *fw_id,
            exec_bit: 5 + idx as u32,
            is_tcb: *is_tcb,
            is_ak_target: false,
            ..Default::default()
        });
        soc_image_paths.push(soc_image_path);
        load_addr += soc_image_fw.len() as u64;
    }

    (soc_images, soc_image_paths)
}

/// Maps a hitless attestation feature to the feature used to build its update payload.
///
/// # Returns
///
/// - `Some(&str)`: The feature whose runtime is shipped in the hitless update package.
/// - `None`: If `feature` is not a hitless attestation feature.
pub fn hitless_attestation_update_feature(feature: &str) -> Option<&'static str> {
    match feature {
        "test-mctp-spdm-attestation-hitless" => Some("test-mctp-spdm-attestation"),
        "test-mctp-spdm-attestation-hitless-tcb" => Some("test-mctp-spdm-attestation-tcb"),
        "test-mctp-spdm-attestation-hitless-mixed" => Some("test-mctp-spdm-attestation-mixed"),
        _ => None,
    }
}

/// Default MCU image configuration for features that need one but did not receive
/// an explicit `--mcu-cfg`.
pub fn default_mcu_image_cfg_for_feature(feature: &str, runtime_path: &Path) -> Option<ImageCfg> {
    if feature != "test-firmware-update-flash" {
        return None;
    }

    Some(ImageCfg {
        path: runtime_path.to_path_buf(),
        load_addr: MCI_BASE_AXI_ADDRESS + MCU_SRAM_OFFSET,
        staging_addr: MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET + (512 * 1024),
        image_id: MCU_RT_IDENTIFIER,
        component_id: MCU_RT_IDENTIFIER,
        exec_bit: 2,
        feature: feature.to_string(),
        ..Default::default()
    })
}

/// Default MCU image configuration for the hitless attestation features.
pub fn default_hitless_attestation_mcu_cfg(feature: &str) -> ImageCfg {
    ImageCfg {
        path: "mcu".into(),
        load_addr: MCI_BASE_AXI_ADDRESS + MCU_SRAM_OFFSET,
        staging_addr: MCI_BASE_AXI_ADDRESS + MCU_MBOX_SRAM1_OFFSET + (512 * 1024),
        image_id: MCU_RT_IDENTIFIER,
        component_id: MCU_RT_IDENTIFIER,
        exec_bit: 2,
        feature: feature.to_string(),
        ..Default::default()
    }
}

/// Pre-generate the target-dir `generated/` configs so the runtime build can
/// embed the integrator-owned static attestation configuration.
pub fn pre_generate_attestation_manifest_config(
    vendor: &str,
    model: &str,
    soc_images: &[ImageCfg],
) -> Result<()> {
    pre_generate_attestation_manifest_config_to_target_dir(
        &crate::target_dir(),
        vendor,
        model,
        soc_images,
    )
}

/// Same as [`pre_generate_attestation_manifest_config`], but for an explicit target directory.
pub fn pre_generate_attestation_manifest_config_to_target_dir(
    target_dir: &Path,
    vendor: &str,
    model: &str,
    soc_images: &[ImageCfg],
) -> Result<()> {
    crate::attestation_manifest::write_config_to_target_dir(
        target_dir,
        vendor,
        model,
        soc_images,
        "pre_generate_attestation_manifest_config",
        "Pre-generated",
    )
}

/// Fingerprint of the generated user-app configuration files in `target_dir`.
///
/// Used to make sure the user-app is rebuilt whenever the generated
/// attestation manifest or SoC image descriptors change.
pub(crate) fn generated_user_app_config_fingerprint(target_dir: &Path) -> Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut fingerprint = String::new();
    for file_name in ["attestation_manifest.toml", "soc_image_descriptors.toml"] {
        let bytes = std::fs::read(target_dir.join("generated").join(file_name))?;
        for byte in bytes {
            fingerprint.push(HEX[(byte >> 4) as usize] as char);
            fingerprint.push(HEX[(byte & 0x0f) as usize] as char);
        }
        fingerprint.push('-');
    }
    Ok(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attestation_soc_image_ids_use_distinct_vendor_namespace() {
        let (base_images, _) = create_default_soc_images();
        let (attestation_images, _) =
            create_attestation_soc_images(Some("test-mctp-spdm-attestation-mixed"));
        let owner_images = create_default_owner_soc_images(&attestation_images);

        let mut image_ids = base_images
            .iter()
            .chain(attestation_images.iter())
            .map(|image| image.image_id)
            .collect::<Vec<_>>();
        image_ids.sort_unstable();
        image_ids.dedup();

        assert_eq!(
            image_ids.len(),
            base_images.len() + attestation_images.len()
        );
        assert!(attestation_images
            .iter()
            .all(|image| (0x1000..0x1_0000).contains(&image.image_id)));
        assert!(owner_images.iter().all(|image| image.image_id >= 0x1_0000));
    }
}
