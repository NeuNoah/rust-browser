//! Fingerprinting protection configuration.
//!
//! Core principle: **never randomize.** A random value per device makes
//! the device *more* unique, not less. Protection means either
//! providing a fixed, deterministic value (shared by all users of this
//! browser version) or blocking the API entirely — never noise.
//!
//! This module is the configuration model only. The actual enforcement
//! inside Servo's script layer is a later phase (see ROADMAP); the
//! model exists now so the privacy surface is explicit and tested.

/// What to do with a fingerprintable API surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtectionLevel {
    /// Leave the API untouched.
    Default,
    /// Return fixed values shared by all browser instances.
    Fixed,
    /// Block the API (returns empty/undefined results).
    Block,
}

/// Fingerprintable surface categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FingerprintCategory {
    /// Canvas `getImageData`/`toDataURL`.
    Canvas,
    /// Web Audio fingerprinting (`AnalyserNode`).
    Audio,
    /// WebGL parameters and extensions.
    Webgl,
    /// Font enumeration (`document.fonts` probing).
    Fonts,
    /// `screen.width/height/colorDepth/...`.
    Screen,
    /// The `User-Agent` header and `navigator.userAgent`.
    UserAgent,
    /// Client hints (`Sec-CH-UA-*`).
    ClientHints,
    /// GPU information (`WEBGL_debug_renderer_info`, `Navigator.gpu`).
    Gpu,
}

/// The full fingerprinting configuration. Defaults prefer fixing over
/// blocking where the web depends on an API, and blocking where the
/// API is purely informational.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintingConfig {
    pub canvas: ProtectionLevel,
    pub audio: ProtectionLevel,
    pub webgl: ProtectionLevel,
    pub fonts: ProtectionLevel,
    pub screen: ProtectionLevel,
    pub user_agent: ProtectionLevel,
    pub client_hints: ProtectionLevel,
    pub gpu: ProtectionLevel,
}

impl Default for FingerprintingConfig {
    fn default() -> Self {
        Self {
            // Canvas is heavily used; a fixed noise-injection constant
            // layer is the long-term plan. Phase 1: leave default.
            canvas: ProtectionLevel::Default,
            audio: ProtectionLevel::Default,
            webgl: ProtectionLevel::Default,
            fonts: ProtectionLevel::Default,
            screen: ProtectionLevel::Default,
            // UA and client hints: the single biggest fingerprint
            // surface; fixed across sessions is a Phase 9 target.
            user_agent: ProtectionLevel::Fixed,
            client_hints: ProtectionLevel::Block,
            gpu: ProtectionLevel::Fixed,
        }
    }
}

impl FingerprintingConfig {
    /// The protection level for a category.
    pub fn level(&self, category: FingerprintCategory) -> ProtectionLevel {
        match category {
            FingerprintCategory::Canvas => self.canvas,
            FingerprintCategory::Audio => self.audio,
            FingerprintCategory::Webgl => self.webgl,
            FingerprintCategory::Fonts => self.fonts,
            FingerprintCategory::Screen => self.screen,
            FingerprintCategory::UserAgent => self.user_agent,
            FingerprintCategory::ClientHints => self.client_hints,
            FingerprintCategory::Gpu => self.gpu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fix_ua_and_block_client_hints() {
        let config = FingerprintingConfig::default();
        assert_eq!(
            config.level(FingerprintCategory::UserAgent),
            ProtectionLevel::Fixed
        );
        assert_eq!(
            config.level(FingerprintCategory::ClientHints),
            ProtectionLevel::Block
        );
        assert_eq!(
            config.level(FingerprintCategory::Canvas),
            ProtectionLevel::Default
        );
    }

    #[test]
    fn levels_are_ordered() {
        assert!(ProtectionLevel::Default < ProtectionLevel::Fixed);
        assert!(ProtectionLevel::Fixed < ProtectionLevel::Block);
    }
}
