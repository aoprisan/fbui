//! Display-backend selection (Phase 6).
//!
//! Which concrete [`Display`](crate::Display) the platform brings up is chosen at
//! **runtime** — from [`PlatformConfig`](crate::PlatformConfig), or the
//! `FBUI_BACKEND` environment variable — rather than baked in at compile time.
//! This is the seam the optional GPU path plugs into: a new backend is just
//! another entry here and another `Display` impl, with the **software path the
//! default and always supported**.
//!
//! The selection logic is a pure function of the request ([`Backend::order`]), so
//! it's unit-testable without touching a device.

/// A backend *preference*. Distinct from
/// [`BackendKind`](crate::BackendKind), which reports what actually came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Try the best available: DRM/KMS dumb buffers, then the fbdev fallback.
    /// (The GPU path is never chosen implicitly — it's opt-in.)
    #[default]
    Auto,
    /// DRM/KMS dumb buffers only; error out rather than fall back.
    DrmDumb,
    /// Legacy fbdev only.
    Fbdev,
    /// The GPU path (DRM + GBM + EGL). Requires the `gpu` feature and a GPU/EGL
    /// host; see `PHASE6.md`.
    Gpu,
}

/// One concrete bring-up attempt, in the order [`Backend::order`] yields them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    DrmDumb,
    Fbdev,
    Gpu,
}

impl Backend {
    /// Parse a backend name (case-insensitive). Accepts the short names and the
    /// fuller aliases. Returns `None` for anything unrecognized.
    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Backend::Auto),
            "drm" | "drm-dumb" | "drmdumb" | "kms" => Some(Backend::DrmDumb),
            "fbdev" | "fb" => Some(Backend::Fbdev),
            "gpu" | "drm-gbm-egl" | "egl" => Some(Backend::Gpu),
            _ => None,
        }
    }

    /// Read the `FBUI_BACKEND` environment variable, falling back to [`Auto`] when
    /// it's unset or unrecognized.
    ///
    /// [`Auto`]: Backend::Auto
    pub fn from_env() -> Backend {
        std::env::var("FBUI_BACKEND")
            .ok()
            .and_then(|v| Backend::parse(&v))
            .unwrap_or(Backend::Auto)
    }

    /// The ordered list of concrete bring-up attempts for this preference. Only
    /// [`Auto`](Backend::Auto) falls back; an explicit choice yields exactly one
    /// attempt, so a misconfiguration fails loudly instead of silently degrading.
    pub fn order(self) -> &'static [Attempt] {
        match self {
            Backend::Auto => &[Attempt::DrmDumb, Attempt::Fbdev],
            Backend::DrmDumb => &[Attempt::DrmDumb],
            Backend::Fbdev => &[Attempt::Fbdev],
            Backend::Gpu => &[Attempt::Gpu],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_names_and_aliases() {
        assert_eq!(Backend::parse("auto"), Some(Backend::Auto));
        assert_eq!(Backend::parse("AUTO"), Some(Backend::Auto));
        assert_eq!(Backend::parse(" drm "), Some(Backend::DrmDumb));
        assert_eq!(Backend::parse("drm-dumb"), Some(Backend::DrmDumb));
        assert_eq!(Backend::parse("fbdev"), Some(Backend::Fbdev));
        assert_eq!(Backend::parse("gpu"), Some(Backend::Gpu));
        assert_eq!(Backend::parse("drm-gbm-egl"), Some(Backend::Gpu));
        assert_eq!(Backend::parse("nonsense"), None);
    }

    #[test]
    fn auto_falls_back_explicit_does_not() {
        assert_eq!(Backend::Auto.order(), &[Attempt::DrmDumb, Attempt::Fbdev]);
        assert_eq!(Backend::DrmDumb.order(), &[Attempt::DrmDumb]);
        assert_eq!(Backend::Fbdev.order(), &[Attempt::Fbdev]);
        assert_eq!(Backend::Gpu.order(), &[Attempt::Gpu]);
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(Backend::default(), Backend::Auto);
    }
}
