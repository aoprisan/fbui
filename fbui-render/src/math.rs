//! Float math that works with and without `std`.
//!
//! `f32::floor`, `sin`, `sqrt` and friends are inherent methods only when `std`
//! is linked; `core` lacks them. [`F32Ext`] supplies the same names backed by
//! `libm`, so code written against the `std` API (`x.floor()`) compiles
//! unchanged in the `no_std` build: import the trait and, under `std`, the
//! inherent methods still win (inherent methods shadow trait methods), so the
//! hosted build's numerics are exactly what they were.
//!
//! Each crate in the stack pulls this in through its private prelude; app code
//! targeting `no_std` can `use fbui_render::math::F32Ext` for the same effect.

/// The `std` float API on `f32`, backed by `libm`. See the [module docs](self).
pub trait F32Ext {
    fn floor(self) -> f32;
    fn ceil(self) -> f32;
    fn round(self) -> f32;
    fn trunc(self) -> f32;
    fn fract(self) -> f32;
    fn abs(self) -> f32;
    fn sqrt(self) -> f32;
    fn sin(self) -> f32;
    fn cos(self) -> f32;
    fn tan(self) -> f32;
    fn atan2(self, other: f32) -> f32;
    fn powf(self, n: f32) -> f32;
    fn powi(self, n: i32) -> f32;
    fn exp(self) -> f32;
    fn ln(self) -> f32;
    fn log10(self) -> f32;
    fn hypot(self, other: f32) -> f32;
    fn rem_euclid(self, rhs: f32) -> f32;
    fn signum(self) -> f32;
}

impl F32Ext for f32 {
    #[inline]
    fn floor(self) -> f32 {
        libm::floorf(self)
    }
    #[inline]
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    #[inline]
    fn round(self) -> f32 {
        libm::roundf(self)
    }
    #[inline]
    fn trunc(self) -> f32 {
        libm::truncf(self)
    }
    #[inline]
    fn fract(self) -> f32 {
        self - libm::truncf(self)
    }
    #[inline]
    fn abs(self) -> f32 {
        libm::fabsf(self)
    }
    #[inline]
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }
    #[inline]
    fn sin(self) -> f32 {
        libm::sinf(self)
    }
    #[inline]
    fn cos(self) -> f32 {
        libm::cosf(self)
    }
    #[inline]
    fn tan(self) -> f32 {
        libm::tanf(self)
    }
    #[inline]
    fn atan2(self, other: f32) -> f32 {
        libm::atan2f(self, other)
    }
    #[inline]
    fn powf(self, n: f32) -> f32 {
        libm::powf(self, n)
    }
    #[inline]
    fn powi(self, n: i32) -> f32 {
        libm::powf(self, n as f32)
    }
    #[inline]
    fn exp(self) -> f32 {
        libm::expf(self)
    }
    #[inline]
    fn ln(self) -> f32 {
        libm::logf(self)
    }
    #[inline]
    fn log10(self) -> f32 {
        libm::log10f(self)
    }
    #[inline]
    fn hypot(self, other: f32) -> f32 {
        libm::hypotf(self, other)
    }
    #[inline]
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = libm::fmodf(self, rhs);
        if r < 0.0 {
            r + libm::fabsf(rhs)
        } else {
            r
        }
    }
    #[inline]
    fn signum(self) -> f32 {
        if self.is_nan() {
            f32::NAN
        } else {
            libm::copysignf(1.0, self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::F32Ext;

    // Call through the trait explicitly: under `std` the inherent methods
    // would otherwise win and these would test nothing.
    #[test]
    fn shim_matches_std() {
        for &x in &[-2.5f32, -0.5, 0.0, 0.4, 1.5, 7.25] {
            assert_eq!(F32Ext::floor(x), x.floor());
            assert_eq!(F32Ext::ceil(x), x.ceil());
            assert_eq!(F32Ext::round(x), x.round());
            assert_eq!(F32Ext::trunc(x), x.trunc());
            assert_eq!(F32Ext::fract(x), x.fract());
            assert_eq!(F32Ext::abs(x), x.abs());
            assert_eq!(F32Ext::signum(x), x.signum());
            assert_eq!(F32Ext::rem_euclid(x, 2.0), x.rem_euclid(2.0));
            assert!((F32Ext::sin(x) - x.sin()).abs() < 1e-6);
        }
    }
}
