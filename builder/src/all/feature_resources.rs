// Licensed under the Apache-2.0 license

//! Feature-specific test resources.
//!
//! This module provides functionality for building feature-specific firmware
//! variants used in integration tests.

use anyhow::Result;
use std::path::PathBuf;
use tempfile::NamedTempFile;

use super::artifacts::BaseArtifacts;
use super::context::AllBuildContext;
use super::flash::{create_flash_image, FlashImageBuildArgs};
use super::pldm::{generate_pldm_package, load_or_create_pldm_manifest};
use super::soc_images::{
    create_attestation_soc_images, create_attestation_soc_images_variant,
    create_default_owner_soc_images, create_default_soc_images, default_mcu_image_cfg_for_feature,
    generated_user_app_config_fingerprint, hitless_attestation_update_feature,
    pre_generate_attestation_manifest_config_to_target_dir,
};
use crate::{CaliptraBuildArgs, CaliptraBuilder, ImageCfg, TARGET};

cfg_if::cfg_if! {
    if #[cfg(feature = "parallel-build")] {
        use rayon::prelude::*;
        fn maybe_par_iter<T: Sync>(slice: &[T]) -> impl ParallelIterator<Item = &T> { slice.par_iter() }
    } else {
        fn maybe_par_iter<T>(slice: &[T]) -> impl Iterator<Item = &T> { slice.iter() }
    }
}

/// Features that require the example app to be included.
/// Determined by which tests use `run_test!(test_name, example_app)` in tests/integration/src/lib.rs
pub const FEATURES_WITH_EXAMPLE_APP: &[&str] = &[
    "test-caliptra-certs",
    "test-get-caliptra-idev-csr",
    "test-caliptra-crypto",
    "test-caliptra-mailbox",
    "test-dma",
    "test-doe-discovery",
    "test-doe-transport-loopback",
    "test-doe-user-loopback",
    "test-external-otp",
    "test-firmware-v2",
    "test-dpe-handle-store",
    "test-sw-pcr-store",
    "test-flash-usermode",
    "test-fpga-flash-ctrl",
    "test-get-device-state",
    "test-log-flash-usermode",
    "test-mbox-sram",
    "test-mci",
    "test-mctp-user-loopback",
    "test-mcu-mbox-soc-requester-loopback",
    "test-mcu-mbox-usermode",
    "test-ocp-lock",
    "test-warm-reset",
];

/// Features that require SoC images to be included in the flash image.
pub const FEATURES_REQUIRING_SOC_IMAGES: &[&str] = &[
    "test-flash-based-boot",
    "test-pldm-streaming-boot",
    "test-firmware-activate",
    "test-firmware-update-flash",
    "test-firmware-update-streaming",
    "test-streaming-boot-flash-write-back",
    "test-mctp-spdm-attestation",
    "test-mctp-spdm-attestation-tcb",
    "test-mctp-spdm-attestation-mixed",
    "test-mctp-spdm-attestation-hitless",
    "test-mctp-spdm-attestation-hitless-tcb",
    "test-mctp-spdm-attestation-hitless-mixed",
    "test-mctp-spdm-attestation-pcr-quote",
    "test-firmware-v2",
    "test-mctp-capsule-loopback",
    "test-mctp-capsule-loopback-warm-reset",
];

/// Features that require flash-based boot (partition table at offset 0).
pub const FEATURES_REQUIRING_FLASH_BOOT: &[&str] =
    &["test-flash-based-boot", "test-firmware-update-flash"];

/// Runtime features whose corresponding ROM feature enables `hw-2-1`.
/// Keep the runtime in sync so large Caliptra mailbox commands use staging SRAM.
const FEATURES_REQUIRING_HW_2_1_RUNTIME: &[&str] = &[
    "test-flash-based-boot",
    "test-firmware-activate",
    "test-firmware-update-flash",
    "test-mctp-spdm-attestation-hitless",
    "test-mctp-spdm-attestation-hitless-tcb",
    "test-mctp-spdm-attestation-hitless-mixed",
    "test-usb-ocp-recovery",
];

/// Features that ship an update flash image (without partition table) in their PLDM package.
const FIRMWARE_UPDATE_FEATURES: &[&str] = &[
    "test-firmware-update-flash",
    "test-firmware-update-streaming",
    "test-streaming-boot-flash-write-back",
    "test-firmware-activate",
    "test-mctp-spdm-attestation-hitless",
    "test-mctp-spdm-attestation-hitless-tcb",
    "test-mctp-spdm-attestation-hitless-mixed",
];

/// Resources built for a specific test feature.
///
/// Each feature gets its own runtime, SoC manifest, flash image, and PLDM package.
pub struct FeatureTestResource {
    /// Feature name this resource was built for.
    pub feature: String,
    /// Temp file containing the runtime binary.
    pub runtime_file: NamedTempFile,
    /// Temp file containing the SoC manifest.
    pub soc_manifest_file: NamedTempFile,
    /// Temp file containing the Owner Authorization manifest, if the feature has Owner SoC images.
    pub owner_auth_manifest_file: Option<NamedTempFile>,
    /// Path to the flash image.
    pub flash_image: PathBuf,
    /// Temp file containing the PLDM firmware package.
    pub pldm_fw_pkg: NamedTempFile,
    /// Optional update flash image (without partition table) for firmware update tests.
    pub update_flash_image: Option<PathBuf>,
    /// SoC manifest that authorizes the payload shipped in the hitless update
    /// package. Only set for the hitless attestation features, where the update
    /// carries a different SoC payload than cold boot.
    pub update_soc_manifest_file: Option<NamedTempFile>,
    /// Raw bytes of the user-app ELF for this feature build. Carries the
    /// `.defmt` table needed by host-side defmt decoders.
    pub user_app_elf: Option<Vec<u8>>,
}

/// Owner-side inputs shared by every flash image built for a feature.
struct OwnerArtifacts {
    /// Owner SoC images.
    soc_images: Option<Vec<ImageCfg>>,
    /// Path to the Owner Authorization manifest.
    auth_manifest_path: Option<PathBuf>,
    /// Path to the Owner measurement policy.
    measurement_policy_path: Option<PathBuf>,
}

/// Build all feature-specific test resources.
///
/// For each feature in the separate_features list, builds a complete set of
/// firmware artifacts including runtime, SoC manifest, flash image, and PLDM package.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `base`: Base artifacts to build upon.
///
/// # Returns
///
/// - `Ok(Vec<FeatureTestResource>)`: List of built feature resources.
/// - `Err(anyhow::Error)`: If any feature build fails.
pub fn build_feature_resources(
    ctx: &AllBuildContext,
    base: &BaseArtifacts,
) -> Result<Vec<FeatureTestResource>> {
    let test_runtimes: Result<Vec<FeatureTestResource>> = maybe_par_iter(&ctx.separate_features)
        .map(|feature| build_single_feature_resource(ctx, base, feature))
        .collect();

    test_runtimes
}

/// Build resources for a single feature.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `base`: Base artifacts to build upon.
/// - `feature`: Feature name to build for.
///
/// # Returns
///
/// - `Ok(FeatureTestResource)`: Built feature resource.
/// - `Err(anyhow::Error)`: If the build fails.
fn build_single_feature_resource(
    ctx: &AllBuildContext,
    base: &BaseArtifacts,
    feature: &str,
) -> Result<FeatureTestResource> {
    // Features that need SoC images get their own target directory, because the
    // generated attestation config embedded by the user-app depends on them.
    let target_dir =
        if cfg!(feature = "parallel-build") || FEATURES_REQUIRING_SOC_IMAGES.contains(&feature) {
            Some(crate::target_dir().join(format!("target-runtime-{}", feature)))
        } else {
            None
        };

    // Generate the descriptor files before compiling this runtime so the
    // user-app build.rs embeds the same SoC image IDs that will be put in the
    // feature-specific SoC manifest below.
    let (feature_soc_images, feature_soc_images_paths) = get_feature_soc_images(ctx, feature)?;
    let feature_owner_soc_images =
        get_feature_owner_soc_images(ctx, feature, feature_soc_images.as_deref());
    let feature_image_load_list = feature_soc_images.clone().unwrap_or_default();
    let generated_target_dir = target_dir.clone().unwrap_or_else(crate::target_dir);
    pre_generate_attestation_manifest_config_to_target_dir(
        &generated_target_dir,
        ctx.effective_vendor(),
        ctx.effective_model(),
        &feature_image_load_list,
    )?;
    let user_app_config_fingerprint = generated_user_app_config_fingerprint(&generated_target_dir)?;

    // Build feature-specific runtime
    let (runtime_file, runtime_path, user_app_elf) =
        build_feature_runtime(ctx, feature, target_dir, user_app_config_fingerprint)?;

    // Get feature-specific MCU image config
    let mcu_image_cfg = ctx
        .get_image_cfg_for_feature(feature)
        .or_else(|| default_mcu_image_cfg_for_feature(feature, &runtime_path));

    // Build Caliptra artifacts with feature-specific runtime
    let mut caliptra_builder = CaliptraBuilder::new(&CaliptraBuildArgs {
        platform: ctx.platform,
        caliptra_rom: Some(base.caliptra_rom.clone()),
        caliptra_firmware: Some(base.caliptra_fw.clone()),
        vendor_pk_hash: Some(base.vendor_pk_hash.clone()),
        mcu_firmware: Some(runtime_path.clone()),
        soc_images: feature_soc_images,
        owner_soc_images: feature_owner_soc_images.clone(),
        mcu_image_cfg: mcu_image_cfg.clone(),
        vendor: ctx.vendor.clone(),
        model: ctx.model.clone(),
        ..Default::default()
    });

    // Generate feature-specific SoC manifest
    let soc_manifest_file = NamedTempFile::new()?;
    caliptra_builder.get_soc_manifest(soc_manifest_file.path().to_str())?;

    // Generate the feature-specific Owner manifest and measurement policy. The
    // temp files must outlive the flash images created below.
    let owner_auth_manifest = if feature_owner_soc_images.is_some() {
        let file = NamedTempFile::new()?;
        let path = caliptra_builder.get_owner_auth_manifest(file.path().to_str())?;
        Some((file, path))
    } else {
        None
    };
    let owner_measurement_policy = if feature_owner_soc_images.is_some() {
        let file = NamedTempFile::new()?;
        let path = caliptra_builder.get_owner_measurement_policy(file.path().to_str())?;
        Some((file, path))
    } else {
        None
    };
    let owner = OwnerArtifacts {
        soc_images: feature_owner_soc_images,
        auth_manifest_path: owner_auth_manifest.as_ref().map(|(_, path)| path.clone()),
        measurement_policy_path: owner_measurement_policy
            .as_ref()
            .map(|(_, path)| path.clone()),
    };

    // Create flash image (with partition table for flash-boot features)
    let is_flash_based_boot = FEATURES_REQUIRING_FLASH_BOOT.contains(&feature);
    let feature_flash_image = create_flash_image(FlashImageBuildArgs {
        caliptra_fw_path: Some(base.caliptra_fw.clone()),
        soc_manifest_path: Some(soc_manifest_file.path().to_path_buf()),
        owner_auth_manifest_path: owner.auth_manifest_path.clone(),
        owner_measurement_policy_path: owner.measurement_policy_path.clone(),
        owner_soc_images: owner.soc_images.clone(),
        mcu_runtime_path: Some(runtime_path.clone()),
        mcu_image_cfg: mcu_image_cfg.clone(),
        caliptra_firmware_network_filename: ctx.caliptra_firmware_network_filename.as_deref(),
        soc_manifest_network_filename: ctx.soc_manifest_network_filename.as_deref(),
        soc_images_paths: feature_soc_images_paths.clone(),
        soc_images: Some(feature_image_load_list.clone()),
        is_flash_based_boot,
    })?;

    // Create update flash image (without partition table) for firmware update features
    let mut update_soc_manifest_file = None;
    let update_flash_image = if FIRMWARE_UPDATE_FEATURES.contains(&feature) {
        if let Some(update_feature) = hitless_attestation_update_feature(feature) {
            let (update_flash, update_soc_manifest) = build_hitless_update_flash_image(
                ctx,
                base,
                feature,
                update_feature,
                mcu_image_cfg.as_ref(),
                &owner,
            )?;
            update_soc_manifest_file = Some(update_soc_manifest);
            Some(update_flash)
        } else {
            Some(create_flash_image(FlashImageBuildArgs {
                caliptra_fw_path: Some(base.caliptra_fw.clone()),
                soc_manifest_path: Some(soc_manifest_file.path().to_path_buf()),
                owner_auth_manifest_path: owner.auth_manifest_path.clone(),
                owner_measurement_policy_path: owner.measurement_policy_path.clone(),
                owner_soc_images: owner.soc_images.clone(),
                mcu_runtime_path: Some(runtime_path),
                mcu_image_cfg,
                caliptra_firmware_network_filename: ctx
                    .caliptra_firmware_network_filename
                    .as_deref(),
                soc_manifest_network_filename: ctx.soc_manifest_network_filename.as_deref(),
                soc_images_paths: feature_soc_images_paths,
                soc_images: Some(feature_image_load_list),
                is_flash_based_boot: false, // No partition table for update image
            })?)
        }
    } else {
        None
    };

    // Create PLDM package (use update image if available)
    let pldm_fw_pkg = create_feature_pldm_package(
        ctx,
        update_flash_image.as_ref().unwrap_or(&feature_flash_image),
    )?;

    Ok(FeatureTestResource {
        feature: feature.to_string(),
        runtime_file,
        soc_manifest_file,
        owner_auth_manifest_file: owner_auth_manifest.map(|(file, _)| file),
        flash_image: feature_flash_image,
        pldm_fw_pkg,
        update_flash_image,
        update_soc_manifest_file,
        user_app_elf,
    })
}

/// Build a feature-specific runtime.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `feature`: Feature name to build for.
/// - `target_dir`: Optional dedicated cargo target directory for this runtime.
/// - `user_app_config_fingerprint`: Fingerprint of the generated user-app configuration.
///
/// # Returns
///
/// - `Ok((NamedTempFile, PathBuf, Option<Vec<u8>>))`: Temp file, path to the built runtime
///   and the bytes of the user-app ELF (if it could be read).
/// - `Err(anyhow::Error)`: If the build fails.
fn build_feature_runtime(
    ctx: &AllBuildContext,
    feature: &str,
    target_dir: Option<PathBuf>,
    user_app_config_fingerprint: String,
) -> Result<(NamedTempFile, PathBuf, Option<Vec<u8>>)> {
    let runtime_file = NamedTempFile::new()?;
    let runtime_path_str = runtime_file.path().to_str().unwrap().to_string();
    let include_example_app = FEATURES_WITH_EXAMPLE_APP.contains(&feature);

    // When the matching ROM feature enables hw-2-1, combine it with the
    // separate runtime feature so the staging SRAM path is enabled.
    let needs_hw_2_1 = ctx.propagate_hw_2_1 || feature_requires_hw_2_1(feature);
    let mut combined_features = if needs_hw_2_1 && feature != "hw-2-1" {
        format!("{},hw-2-1", feature)
    } else {
        feature.to_string()
    };

    if ctx.is_release && !combined_features.contains("release") {
        combined_features = format!("{},release", combined_features);
    }

    crate::runtime_build_with_apps(&CaliptraBuildArgs {
        features: Some(&combined_features),
        output_name: Some(runtime_path_str),
        example_app: include_example_app,
        platform: ctx.platform,
        profile: ctx.profile.as_deref(),
        target_dir: target_dir.clone(),
        no_default_features: true,
        user_app_config_fingerprint: Some(user_app_config_fingerprint),
        ..Default::default()
    })?;

    // Capture the user-app ELF bytes for this feature so host-side defmt
    // decoders can read them out of the firmware bundle. Without
    // `parallel-build`, all features share `target/<TARGET>/<profile>/` and
    // overwrite each other, so we must read immediately.
    let user_app_profile = ctx.profile.as_deref().unwrap_or("devel");
    let user_app_target_dir = target_dir.unwrap_or_else(crate::target_dir);
    let user_app_elf_path = user_app_target_dir
        .join(TARGET)
        .join(user_app_profile)
        .join("user-app");
    let user_app_elf = std::fs::read(&user_app_elf_path).ok();

    let runtime_path = runtime_file.path().to_path_buf();
    Ok((runtime_file, runtime_path, user_app_elf))
}

/// Whether the runtime for `feature` needs `hw-2-1` to match its ROM feature.
fn feature_requires_hw_2_1(feature: &str) -> bool {
    FEATURES_REQUIRING_HW_2_1_RUNTIME.contains(&feature)
}

/// Build the update flash image for a hitless attestation feature.
///
/// The update package carries a runtime built for `update_feature` and a
/// different SoC payload than cold boot, so the post-update evidence carries
/// different component digests.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `base`: Base artifacts to build upon.
/// - `feature`: The hitless attestation feature being built.
/// - `update_feature`: The feature the update runtime is built for.
/// - `mcu_image_cfg`: MCU image configuration of `feature`.
/// - `owner`: Owner-side inputs shared with the cold-boot flash image.
///
/// # Returns
///
/// - `Ok((PathBuf, NamedTempFile))`: The update flash image and the SoC manifest authorizing its payload.
///   The manifest is retained so `corim gen-refval --post-update` can derive
///   post-update reference values from it.
/// - `Err(anyhow::Error)`: If any build step fails.
fn build_hitless_update_flash_image(
    ctx: &AllBuildContext,
    base: &BaseArtifacts,
    feature: &str,
    update_feature: &str,
    mcu_image_cfg: Option<&ImageCfg>,
    owner: &OwnerArtifacts,
) -> Result<(PathBuf, NamedTempFile)> {
    let update_target_dir = crate::target_dir().join(format!("target-runtime-update-{}", feature));
    let update_runtime_file = NamedTempFile::new()?;
    let update_runtime_path = update_runtime_file.path().to_str().unwrap().to_string();
    let update_features = format!("{update_feature},hw-2-1");

    // Ship a different SoC payload in the update package so the post-update
    // evidence carries different component digests.
    let (update_soc_images, update_soc_images_paths) =
        create_attestation_soc_images_variant(Some(feature), true);
    pre_generate_attestation_manifest_config_to_target_dir(
        &update_target_dir,
        ctx.effective_vendor(),
        ctx.effective_model(),
        &update_soc_images,
    )?;
    crate::runtime_build_with_apps(&CaliptraBuildArgs {
        features: Some(&update_features),
        output_name: Some(update_runtime_path),
        example_app: FEATURES_WITH_EXAMPLE_APP.contains(&update_feature),
        platform: ctx.platform,
        profile: ctx.profile.as_deref(),
        target_dir: Some(update_target_dir),
        no_default_features: true,
        ..Default::default()
    })?;

    let mut update_builder = CaliptraBuilder::new(&CaliptraBuildArgs {
        platform: ctx.platform,
        caliptra_rom: Some(base.caliptra_rom.clone()),
        caliptra_firmware: Some(base.caliptra_fw.clone()),
        vendor_pk_hash: Some(base.vendor_pk_hash.clone()),
        mcu_firmware: Some(update_runtime_file.path().to_path_buf()),
        soc_images: Some(update_soc_images.clone()),
        mcu_image_cfg: mcu_image_cfg.cloned(),
        vendor: ctx.vendor.clone(),
        model: ctx.model.clone(),
        ..Default::default()
    });
    let update_soc_manifest_file = NamedTempFile::new()?;
    update_builder.get_soc_manifest(update_soc_manifest_file.path().to_str())?;

    let update_flash = create_flash_image(FlashImageBuildArgs {
        caliptra_fw_path: Some(base.caliptra_fw.clone()),
        soc_manifest_path: Some(update_soc_manifest_file.path().to_path_buf()),
        owner_auth_manifest_path: owner.auth_manifest_path.clone(),
        owner_measurement_policy_path: owner.measurement_policy_path.clone(),
        owner_soc_images: owner.soc_images.clone(),
        mcu_runtime_path: Some(update_runtime_file.path().to_path_buf()),
        mcu_image_cfg: mcu_image_cfg.cloned(),
        caliptra_firmware_network_filename: ctx.caliptra_firmware_network_filename.as_deref(),
        soc_manifest_network_filename: ctx.soc_manifest_network_filename.as_deref(),
        soc_images_paths: update_soc_images_paths,
        soc_images: Some(update_soc_images),
        is_flash_based_boot: false,
    })?;

    Ok((update_flash, update_soc_manifest_file))
}

/// Get SoC images for a feature, creating defaults if needed.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `feature`: Feature name.
///
/// # Returns
///
/// - `Ok((Option<Vec<ImageCfg>>, Vec<PathBuf>))`: SoC images config and paths.
/// - `Err(anyhow::Error)`: If image creation fails.
fn get_feature_soc_images(
    ctx: &AllBuildContext,
    feature: &str,
) -> Result<(Option<Vec<ImageCfg>>, Vec<PathBuf>)> {
    if FEATURES_REQUIRING_SOC_IMAGES.contains(&feature) && ctx.soc_images.is_none() {
        let (images, paths) = if feature.starts_with("test-mctp-spdm-attestation") {
            create_attestation_soc_images(Some(feature))
        } else {
            create_default_soc_images()
        };
        Ok((Some(images), paths))
    } else {
        Ok((ctx.soc_images.clone(), ctx.get_soc_image_paths()))
    }
}

/// Get Owner SoC images for a feature, creating defaults if needed.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `feature`: Feature name.
/// - `soc_images`: The vendor SoC images of the feature.
///
/// # Returns
///
/// - `Some(Vec<ImageCfg>)`: The configured Owner SoC images, or defaults for features requiring SoC images.
/// - `None`: If the feature has no Owner SoC images.
fn get_feature_owner_soc_images(
    ctx: &AllBuildContext,
    feature: &str,
    soc_images: Option<&[ImageCfg]>,
) -> Option<Vec<ImageCfg>> {
    if let Some(images) = &ctx.owner_soc_images {
        Some(images.clone())
    } else if FEATURES_REQUIRING_SOC_IMAGES.contains(&feature) {
        Some(create_default_owner_soc_images(soc_images.unwrap_or(&[])))
    } else {
        None
    }
}

/// Create a PLDM package for a feature's flash image.
///
/// # Arguments
///
/// - `ctx`: Build context with configuration.
/// - `flash_image_path`: Path to the flash image to package.
///
/// # Returns
///
/// - `Ok(NamedTempFile)`: Temp file containing the PLDM package.
/// - `Err(anyhow::Error)`: If package creation fails.
fn create_feature_pldm_package(
    ctx: &AllBuildContext,
    flash_image_path: &PathBuf,
) -> Result<NamedTempFile> {
    let flash_image_data = std::fs::read(flash_image_path)?;
    let pldm_manifest =
        load_or_create_pldm_manifest(ctx.pldm_manifest.as_deref(), &flash_image_data)?;

    let pldm_fw_pkg = NamedTempFile::new()?;
    generate_pldm_package(&pldm_manifest, pldm_fw_pkg.path())?;

    Ok(pldm_fw_pkg)
}
