// Licensed under the Apache-2.0 license

//! Build context for the all-in-one firmware build process.
//!
//! This module provides the `AllBuildContext` struct which encapsulates
//! the configuration and state needed throughout the build process.

use super::soc_images::{default_hitless_attestation_mcu_cfg, hitless_attestation_update_feature};
use crate::ImageCfg;

#[derive(Default)]
pub struct AllBuildArgs {
    pub output: Option<String>,
    pub platform: Option<String>,
    pub rom_features: Option<String>,
    /// Network ROM variants to build (comma-separated).
    pub network_rom_features: Option<String>,
    /// Feature flags for runtime build (comma-separated).
    pub runtime_features: Option<String>,
    /// Build separate runtimes for each feature flag.
    pub separate_runtimes: bool,
    pub soc_images: Option<Vec<ImageCfg>>,
    /// Owner-only SoC images included in the Owner Authorization Manifest.
    pub owner_soc_images: Option<Vec<ImageCfg>>,
    pub mcu_cfgs: Option<Vec<ImageCfg>>,
    pub caliptra_firmware_network_filename: Option<String>,
    pub soc_manifest_network_filename: Option<String>,
    pub pldm_manifest: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// Cargo profile name to build with (e.g. `"release"` or `"devel"`).
    pub profile: Option<String>,
    /// Index of the shard to build. Only used when `total_shards > 1`.
    pub shard_index: usize,
    /// Number of shards the separate runtimes are split into.
    pub total_shards: usize,
}

/// Build context holding shared configuration for the build process.
///
/// This struct encapsulates all the configuration needed throughout
/// the firmware build process, avoiding the need to thread parameters
/// through multiple function calls.
pub struct AllBuildContext {
    pub platform: String,
    /// Whether building for FPGA (vs emulator).
    pub fpga: bool,
    /// Feature flags for ROM build.
    pub rom_features: String,
    /// Network ROM variants to build (comma-separated).
    pub network_rom_features: Option<String>,
    /// SoC images configuration.
    pub soc_images: Option<Vec<ImageCfg>>,
    /// Owner SoC images configuration.
    pub owner_soc_images: Option<Vec<ImageCfg>>,
    /// MCU configuration images.
    pub mcu_cfgs: Option<Vec<ImageCfg>>,
    /// File name of the Caliptra firmware in the flash image's network boot section.
    pub caliptra_firmware_network_filename: Option<String>,
    /// File name of the SoC manifest in the flash image's network boot section.
    pub soc_manifest_network_filename: Option<String>,
    /// Path to custom PLDM manifest.
    pub pldm_manifest: Option<String>,
    /// Vendor name, as passed by the caller.
    pub vendor: Option<String>,
    /// Model name, as passed by the caller.
    pub model: Option<String>,
    /// Cargo profile name.
    pub profile: Option<String>,
    /// Whether the `release` profile is being built.
    pub is_release: bool,
    /// Whether `hw-2-1` is propagated from the ROM features to the runtime builds.
    pub propagate_hw_2_1: bool,
    /// Index of the shard being built.
    pub shard_index: usize,
    /// Total number of shards (at least 1).
    pub total_shards: usize,
    /// Output path for the firmware bundle.
    pub output: Option<String>,
    /// Base runtime features (used when not building separate runtimes).
    pub base_runtime_features: Vec<String>,
    /// Separate features (each gets its own runtime).
    pub separate_features: Vec<String>,
}

impl AllBuildContext {
    /// Create a new build context from build arguments.
    ///
    /// # Arguments
    ///
    /// - `args`: Build arguments containing configuration options.
    ///
    /// # Returns
    ///
    /// - `AllBuildContext`: Initialized build context.
    pub fn new(args: AllBuildArgs) -> Self {
        let platform = args.platform.unwrap_or_else(|| "emulator".to_string());
        let fpga = platform == "fpga";
        let rom_features = args.rom_features.unwrap_or_default();
        let total_shards = args.total_shards.max(1);

        // Auto-enable the `release` cargo feature when building with the `release`
        // profile, mirroring the same logic in `xtask runtime-build`. This strips
        // kernel `debug!()`, romtime `println!()`, Console, DebugWriter, etc.
        let is_release = args.profile.as_deref() == Some("release");

        // Determine runtime features
        let mut runtime_features: Vec<String> = match args.runtime_features {
            Some(r) if !r.is_empty() => r.split(',').map(|s| s.to_string()).collect(),
            _ => {
                if args.separate_runtimes {
                    let features = if is_release {
                        crate::features::RELEASE_RUNTIME_TEST_FEATURES
                    } else if fpga {
                        crate::features::FPGA_RUNTIME_TEST_FEATURES
                    } else {
                        crate::features::EMULATOR_RUNTIME_TEST_FEATURES
                    };
                    features.iter().map(|s| s.to_string()).collect()
                } else {
                    vec![]
                }
            }
        };

        // Propagate hw-2-1 from rom_features to runtime builds so that
        // staging SRAM is enabled (required when ImageManifest > 16 KB).
        let propagate_hw_2_1 = rom_features.contains("hw-2-1");
        if propagate_hw_2_1 && !runtime_features.iter().any(|f| f == "hw-2-1") {
            runtime_features.push("hw-2-1".to_string());
        }

        let (mut base_runtime_features, separate_features) = if args.separate_runtimes {
            // Build a separate runtime for each feature flag, since they are used as tests
            let separate_features = if total_shards > 1 {
                runtime_features
                    .into_iter()
                    .enumerate()
                    .filter(|(idx, _)| idx % total_shards == args.shard_index)
                    .map(|(_, feature)| feature)
                    .collect()
            } else {
                runtime_features
            };
            (vec![], separate_features)
        } else {
            // Build one runtime with all feature flags
            (runtime_features, vec![])
        };

        if is_release && !base_runtime_features.iter().any(|f| f == "release") {
            base_runtime_features.push("release".to_string());
        }

        Self {
            platform,
            fpga,
            rom_features,
            network_rom_features: args.network_rom_features,
            soc_images: args.soc_images,
            owner_soc_images: args.owner_soc_images,
            mcu_cfgs: args.mcu_cfgs,
            caliptra_firmware_network_filename: args.caliptra_firmware_network_filename,
            soc_manifest_network_filename: args.soc_manifest_network_filename,
            pldm_manifest: args.pldm_manifest,
            vendor: args.vendor,
            model: args.model,
            profile: args.profile,
            is_release,
            propagate_hw_2_1,
            shard_index: args.shard_index,
            total_shards,
            output: args.output,
            base_runtime_features,
            separate_features,
        }
    }

    /// Vendor name used for the generated attestation configuration.
    pub fn effective_vendor(&self) -> &str {
        self.vendor.as_deref().unwrap_or("ChipsAlliance")
    }

    /// Model name used for the generated attestation configuration.
    pub fn effective_model(&self) -> &str {
        self.model.as_deref().unwrap_or("Caliptra-SS")
    }

    /// Get the image configuration for a specific feature.
    ///
    /// Hitless attestation features fall back to a default MCU image configuration.
    ///
    /// # Arguments
    ///
    /// - `feature`: Feature name to look up.
    ///
    /// # Returns
    ///
    /// - `Option<ImageCfg>`: The matching image configuration, if found.
    pub fn get_image_cfg_for_feature(&self, feature: &str) -> Option<ImageCfg> {
        self.mcu_cfgs
            .as_ref()
            .and_then(|cfgs| cfgs.iter().find(|img| img.feature == feature).cloned())
            .or_else(|| {
                hitless_attestation_update_feature(feature)
                    .map(|_| default_hitless_attestation_mcu_cfg(feature))
            })
    }

    /// Get SoC image paths from the configuration.
    ///
    /// # Returns
    ///
    /// - `Vec<std::path::PathBuf>`: Paths to all configured SoC images.
    pub fn get_soc_image_paths(&self) -> Vec<std::path::PathBuf> {
        self.soc_images
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|img| img.path.clone())
            .collect()
    }
}
