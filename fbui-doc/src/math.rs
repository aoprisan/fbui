//! Float math for a `no_std` crate: the `std` f32 API backed by `libm`
//! (the same shim as `fbui_render::math`, kept local so this crate stays
//! independent of the fbui stack).

/// The `std` float API on `f32`, backed by `libm`.
#[allow(dead_code)]
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
