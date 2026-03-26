// Licensed under the Apache-2.0 license

//! Base firmware artifacts.
//!
//! This module defines the `BaseArtifacts` struct which holds all the
//! core firmware components built during the all-in-one build process.

use anyhow::Result;
use std::path::PathBuf;
use tempfile::NamedTempFile;

use super::context::AllBuildContext;
use super::feature_resources::FEATURES_REQUIRING_SOC_IMAGES;
use super::flash::{create_flash_image, FlashImageBuildArgs};
use super::pldm::{generate_pldm_package, load_or_create_pldm_manifest};
use super::soc_images::{
    create_attestation_soc_images, create_default_owner_soc_images, create_default_soc_images,
    generated_user_app_config_fingerprint, pre_generate_attestation_manifest_config,
};
use crate::{CaliptraBuildArgs, CaliptraBuilder, ImageCfg, TARGET};

/// Base firmware artifacts produced by the build process.
///
/// This struct holds paths to all the core firmware components that are
/// built once and potentially reused when building feature-specific variants.
pub struct BaseArtifacts {
    /// Path to the Caliptra ROM binary.
    pub caliptra_rom: PathBuf,
    /// Path to the Caliptra firmware bundle.
    pub caliptra_fw: PathBuf,
    /// Path to the Caliptra firmware bundle with SVN 7.
    pub caliptra_fw_svn7: PathBuf,
    /// Path to the Caliptra firmware bundle with SVN 128.
    pub caliptra_fw_svn128: PathBuf,
    /// Path to the Caliptra firmware bundle signed with the second key.
    pub caliptra_fw_key2: PathBuf,
    /// Path to the Caliptra firmware bundle with OCP LOCK enabled.
    pub caliptra_fw_ocp_lock: PathBuf,
    /// Vendor public key hash (hex string).
    pub vendor_pk_hash: String,
    /// Path to the MCU ROM binary.
    pub mcu_rom: PathBuf,
    /// Temp file holding a copy of the MCU ROM (to keep it alive).
    ///
    /// Feature ROM builds reuse the bundler output path of the base ROM, so the
    /// base ROM is preserved before any of them run.
    pub _mcu_rom_file: NamedTempFile,
    /// Path to the MCU runtime binary.
    pub mcu_runtime: PathBuf,
    /// Temp file holding the MCU runtime (to keep it alive).
    pub _mcu_runtime_file: NamedTempFile,
    /// Raw bytes of the base user-app ELF. Carries the `.defmt` table needed by
    /// host-side defmt decoders.
    pub user_app_elf: Vec<u8>,
    /// Bare-metal binaries: (package name, path).
    pub bare_metal_paths: Vec<(String, PathBuf)>,
    /// Path to the SoC manifest.
    pub soc_manifest: PathBuf,
    /// Path to the Owner Authorization manifest.
    pub owner_auth_manifest: PathBuf,
    /// Path to the flash image.
    pub flash_image: PathBuf,
    /// Path to the PLDM firmware package.
    pub pldm_fw_pkg: PathBuf,
    /// Temp file holding the PLDM package (to keep it alive).
    pub _pldm_fw_pkg_file: NamedTempFile,
}

impl BaseArtifacts {
    /// Build all base artifacts from the given context.
    ///
    /// # Arguments
    ///
    /// - `ctx`: Build context with configuration.
    ///
    /// # Returns
    ///
    /// - `Ok(BaseArtifacts)`: All built artifacts.
    /// - `Err(anyhow::Error)`: If any build step fails.
    pub fn build(ctx: &AllBuildContext) -> Result<Self> {
        // Build MCU ROM
        let built_mcu_rom = crate::rom_build(&CaliptraBuildArgs {
            platform: Some(&ctx.platform),
            features: Some(&ctx.rom_features),
            ..Default::default()
        })?;
        let mcu_rom_file = NamedTempFile::new()?;
        std::fs::copy(&built_mcu_rom, mcu_rom_file.path())?;
        let mcu_rom = mcu_rom_file.path().to_path_buf();

        // Determine effective SoC images for the base runtime features.
        // When the caller didn't supply --soc_image args, auto-create them for
        // features that need SoC images (e.g. attestation).
        let effective_soc_images = effective_base_soc_images(ctx);

        pre_generate_attestation_manifest_config(
            ctx.effective_vendor(),
            ctx.effective_model(),
            effective_soc_images.as_deref().unwrap_or(&[]),
        )?;
        let base_user_app_config_fingerprint =
            generated_user_app_config_fingerprint(&crate::target_dir())?;

        // Build base MCU runtime
        let runtime_file = NamedTempFile::new()?;
        let runtime_path = runtime_file.path().to_str().unwrap().to_string();

        let base_features = if ctx.base_runtime_features.is_empty() {
            ctx.propagate_hw_2_1.then(|| "hw-2-1".to_string())
        } else {
            Some(ctx.base_runtime_features.join(","))
        };

        // For FPGA devel builds with no explicit base-runtime features (i.e.
        // separate-runtimes mode where every test gets its own runtime), suppress
        // the crate's `default = ["all-features"]` because that would include
        // streaming-boot, flash-boot, firmware-update, etc. and overflow the
        // FPGA's 512 KB SRAM. Tests that need services such as SPDM should use an
        // explicit test feature so the corresponding separate runtime enables it.
        // Release builds (any platform) also suppress defaults: `all-features`
        // pulls in SPDM, streaming-boot, firmware-update, etc. on top of `release`
        // and overflows both the 512 KB FPGA and emulator release SRAM budgets.
        // Feature-specific release test runtimes use `--no-default-features`
        // too, so `release` must not pull in unrelated services such as
        // firmware-update.
        let no_default_features =
            ctx.is_release || (ctx.fpga && ctx.base_runtime_features.is_empty());

        let mcu_runtime = crate::runtime_build_with_apps(&CaliptraBuildArgs {
            features: base_features.as_deref(),
            output_name: Some(runtime_path),
            example_app: false,
            platform: Some(&ctx.platform),
            profile: ctx.profile.as_deref(),
            no_default_features,
            user_app_config_fingerprint: Some(base_user_app_config_fingerprint),
            ..Default::default()
        })?;

        let user_app_profile = ctx.profile.as_deref().unwrap_or("devel");
        let user_app_elf_path = crate::target_dir()
            .join(TARGET)
            .join(user_app_profile)
            .join("user-app");
        let user_app_elf = std::fs::read(&user_app_elf_path)?;

        let mut bare_metal_paths = vec![];
        for package in crate::features::BARE_METAL_BINARIES {
            let path = crate::bare_metal_build(Some(&ctx.platform), package)?;
            bare_metal_paths.push((package.to_string(), path));
        }

        // Build Caliptra artifacts
        let mcu_image_cfg = ctx.get_image_cfg_for_feature("none");
        // Caliptra currently rejects owner manifests with an empty metadata collection.
        let owner_soc_images = ctx.owner_soc_images.clone().unwrap_or_else(|| {
            create_default_owner_soc_images(effective_soc_images.as_deref().unwrap_or(&[]))
        });
        let mut caliptra_builder = CaliptraBuilder::new(&CaliptraBuildArgs {
            fpga: ctx.fpga,
            mcu_firmware: Some(mcu_runtime.clone()),
            soc_images: effective_soc_images.clone(),
            owner_soc_images: Some(owner_soc_images.clone()),
            mcu_image_cfg: mcu_image_cfg.clone(),
            vendor: ctx.vendor.clone(),
            model: ctx.model.clone(),
            ..Default::default()
        });

        let caliptra_rom = caliptra_builder.get_caliptra_rom()?;
        let caliptra_fw = caliptra_builder.get_caliptra_fw()?;
        let vendor_pk_hash = caliptra_builder.get_vendor_pk_hash()?.to_string();
        println!("Vendor PK hash: {:x?}", vendor_pk_hash);

        let soc_manifest = caliptra_builder.get_soc_manifest(None)?;
        let owner_auth_manifest = caliptra_builder.get_owner_auth_manifest(None)?;
        let owner_measurement_policy = caliptra_builder.get_owner_measurement_policy(None)?;

        // Caliptra firmware variants used by the firmware update and rollback tests
        let caliptra_fw_svn7 = build_caliptra_fw_variant(CaliptraBuildArgs {
            fpga: ctx.fpga,
            svn: Some(7),
            ..Default::default()
        })?;
        let caliptra_fw_svn128 = build_caliptra_fw_variant(CaliptraBuildArgs {
            fpga: ctx.fpga,
            svn: Some(128),
            ..Default::default()
        })?;
        let caliptra_fw_key2 = build_caliptra_fw_variant(CaliptraBuildArgs {
            fpga: ctx.fpga,
            use_second_key: true,
            ..Default::default()
        })?;
        let caliptra_fw_ocp_lock = build_caliptra_fw_variant(CaliptraBuildArgs {
            fpga: ctx.fpga,
            ocp_lock: true,
            ..Default::default()
        })?;

        // Create flash image (not for flash-based boot)
        let flash_image = create_flash_image(FlashImageBuildArgs {
            caliptra_fw_path: Some(caliptra_fw.clone()),
            soc_manifest_path: Some(soc_manifest.clone()),
            owner_auth_manifest_path: Some(owner_auth_manifest.clone()),
            owner_measurement_policy_path: Some(owner_measurement_policy),
            owner_soc_images: Some(owner_soc_images),
            mcu_runtime_path: Some(mcu_runtime.clone()),
            mcu_image_cfg,
            caliptra_firmware_network_filename: ctx.caliptra_firmware_network_filename.as_deref(),
            soc_manifest_network_filename: ctx.soc_manifest_network_filename.as_deref(),
            soc_images_paths: effective_soc_images
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|img| img.path.clone())
                .collect(),
            soc_images: effective_soc_images,
            is_flash_based_boot: false,
        })?;

        // Create PLDM firmware package
        let flash_image_data = std::fs::read(&flash_image)?;
        let pldm_manifest =
            load_or_create_pldm_manifest(ctx.pldm_manifest.as_deref(), &flash_image_data)?;

        let pldm_fw_pkg_file = NamedTempFile::new()?;
        generate_pldm_package(&pldm_manifest, pldm_fw_pkg_file.path())?;

        Ok(Self {
            caliptra_rom,
            caliptra_fw,
            caliptra_fw_svn7,
            caliptra_fw_svn128,
            caliptra_fw_key2,
            caliptra_fw_ocp_lock,
            vendor_pk_hash,
            mcu_rom,
            _mcu_rom_file: mcu_rom_file,
            mcu_runtime,
            _mcu_runtime_file: runtime_file,
            user_app_elf,
            bare_metal_paths,
            soc_manifest,
            owner_auth_manifest,
            flash_image,
            pldm_fw_pkg: pldm_fw_pkg_file.path().to_path_buf(),
            _pldm_fw_pkg_file: pldm_fw_pkg_file,
        })
    }
}

/// Determine the SoC images used by the base runtime.
///
/// If the caller supplied SoC images they are used as is. Otherwise default
/// images are created when any of the base runtime features requires them.
fn effective_base_soc_images(ctx: &AllBuildContext) -> Option<Vec<ImageCfg>> {
    if ctx.soc_images.is_some() {
        return ctx.soc_images.clone();
    }

    let needs_soc = ctx
        .base_runtime_features
        .iter()
        .any(|f| FEATURES_REQUIRING_SOC_IMAGES.contains(&f.as_str()));
    if !needs_soc {
        return None;
    }

    let attestation_feature = ctx
        .base_runtime_features
        .iter()
        .find(|f| f.starts_with("test-mctp-spdm-attestation"));
    let (images, _paths) = match attestation_feature {
        Some(feature) => create_attestation_soc_images(Some(feature)),
        None => create_default_soc_images(),
    };
    Some(images)
}

/// Build a Caliptra firmware bundle variant.
fn build_caliptra_fw_variant(args: CaliptraBuildArgs) -> Result<PathBuf> {
    CaliptraBuilder::new(&args).get_caliptra_fw()
}
