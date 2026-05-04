// SPDX-License-Identifier: GPL-2.0

//! Helper types and utilities

macro_rules! endian_type {
    ($native_type:ident, $wrapper_type:ident, $to_wrapper:ident, $from_wrapper:ident) => {
        /// An unsigned integer type with an explicit endianness.
        #[derive(Copy, Clone, Eq, PartialEq, Debug, Default, pin_init::Zeroable)]
        #[repr(transparent)]
        pub struct $wrapper_type($native_type);

        $crate::static_assert!(
            ::core::mem::align_of::<$wrapper_type>() == ::core::mem::align_of::<$native_type>()
        );
        $crate::static_assert!(
            ::core::mem::size_of::<$wrapper_type>() == ::core::mem::size_of::<$native_type>()
        );

        impl $wrapper_type {
            #[inline]
            /// Convert to CPU/native endianness.
            pub const fn to_cpu(self) -> $native_type {
                $native_type::$from_wrapper(self.0)
            }
        }

        impl PartialEq<$native_type> for $wrapper_type {
            #[inline]
            fn eq(&self, other: &$native_type) -> bool {
                self.to_cpu() == *other
            }
        }

        impl PartialEq<$wrapper_type> for $native_type {
            #[inline]
            fn eq(&self, other: &$wrapper_type) -> bool {
                *self == other.to_cpu()
            }
        }

        impl From<$wrapper_type> for $native_type {
            #[inline]
            fn from(v: $wrapper_type) -> $native_type {
                v.to_cpu()
            }
        }

        impl From<$native_type> for $wrapper_type {
            #[inline]
            fn from(v: $native_type) -> $wrapper_type {
                $wrapper_type($native_type::$to_wrapper(v))
            }
        }
    };
}

endian_type!(u16, Le16, to_le, from_le);
endian_type!(u32, Le32, to_le, from_le);
endian_type!(u64, Le64, to_le, from_le);
endian_type!(u16, Be16, to_be, from_be);
endian_type!(u32, Be32, to_be, from_be);
endian_type!(u64, Be64, to_be, from_be);

use macros::kunit_tests;

#[kunit_tests(rust_kernel_virtio_endianness)]
mod tests {
    use super::*;
    use kernel::prelude::*;

    #[test]
    fn virtio_endianness() -> Result {
        let n = 42_u64;

        assert_eq!(u64::from(Le64::from(n)), n);
        assert_eq!(u64::from(Be64::from(n)), n);

        assert_eq!(n.to_be(), Be64::from(n).0);
        assert_eq!(n.to_le(), Le64::from(n).0);

        assert_eq!(Le64::from(n).0.to_ne_bytes(), [0x2a, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(Be64::from(n).0.to_ne_bytes(), [0, 0, 0, 0, 0, 0, 0, 0x2a]);

        Ok(())
    }
}
