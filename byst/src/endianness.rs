//! [Endianness](https://en.wikipedia.org/wiki/Endianness)
//!
//! # TODO
//!
//! - Remove `Size`, `Encode` and `Decode` traits as they require unstable
//!   features, and we don't really need them.

use super::rw::{
    End,
    Full,
    ReadIntoBuf,
    ReadXe,
    WriteFromBuf,
    WriteXe,
};

/// Note: Although the [`endianness`][`self`] module is not public, we seal this
/// into yet another module, in case we want to make the [`endianness`](self)
/// module public in future. Just to be safe :)
mod sealed {
    pub trait Sealed {}
}

/// Trait for types that represent endianesses.
///
/// This trait is sealed and can't be implemented for custom types. It is only
/// implemented for [`BigEndian`] and [`LittleEndian`] (and their type aliases).
pub trait Endianness: sealed::Sealed {}

/// Big endian byte order
pub enum BigEndian {}
impl Endianness for BigEndian {}
impl sealed::Sealed for BigEndian {}

/// Little endian byte order
pub enum LittleEndian {}
impl Endianness for LittleEndian {}
impl sealed::Sealed for LittleEndian {}

/// System native byte order.
///
/// On the system that generated these docs, this is little endian.
#[cfg(target_endian = "little")]
pub type NativeEndian = LittleEndian;

/// System native byte order.
///
/// On the system that generated these docs, this is big endian.
#[cfg(target_endian = "big")]
pub type NativeEndian = BigEndian;

/// Network byte order.
///
/// This is always big endian.
pub type NetworkEndian = BigEndian;

/// Trait defining what length in bytes.
pub trait Size {
    const BYTES: usize;
    const BITS: usize;
}

/// Trait for types that can be encoded using a specified endianness.
pub trait Encode<E: Endianness>: Size {
    fn encode(&self) -> [u8; <Self as Size>::BYTES];
}

/// Trait for types that can be decoded using a specified endianness.
pub trait Decode<E: Endianness>: Size {
    fn decode(bytes: &[u8; <Self as Size>::BYTES]) -> Self;
}

// this implements `Encode` and `Decode` for integer (and float) types from
// [`core`].
macro_rules! impl_endianness {
    {
        $(
            $ty:ty : $bytes:expr;
        )*
    } => {
        $(
            impl Size for $ty {
                const BYTES: usize = $bytes;
                const BITS: usize = $bytes * 8;
            }

            impl_endianness!(for<BigEndian> $ty: $bytes => from_be_bytes, to_be_bytes);
            impl_endianness!(for<LittleEndian> $ty: $bytes => from_le_bytes, to_le_bytes);
        )*
    };
    (for<$endianness:ty> $ty:ty: $bytes:expr => $from_bytes:ident, $to_bytes:ident) => {
        impl Encode<$endianness> for $ty {
            #[inline]
            fn encode(&self) -> [u8; $bytes] {
                <$ty>::$to_bytes(*self)
            }
        }

        impl Decode<$endianness> for $ty {
            #[inline]
            fn decode(bytes: &[u8; $bytes]) -> Self {
                <$ty>::$from_bytes(*bytes)
            }
        }

        impl<R: ReadIntoBuf> ReadXe<R, $endianness> for $ty
        {
            #[inline]
            fn read(mut reader: R) -> Result<Self, End> {
                let mut buf = [0u8; $bytes];
                reader.read_into_buf(&mut buf)?;
                Ok(<$ty>::$from_bytes(buf))
            }
        }

        impl<W: WriteFromBuf> WriteXe<W, $endianness> for $ty {
            #[inline]
            fn write(&self, mut writer: W) -> Result<(), Full> {
                let buf = <$ty>::$to_bytes(*self);
                writer.write_from_buf(&buf)
            }
        }
    };
}

impl_endianness! {
    u16: 2;
    i16: 2;
    u32: 4;
    i32: 4;
    u64: 8;
    i64: 8;
    u128: 16;
    i128: 16;
    f32: 4;
    f64: 8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexdump::Hexdump;

    macro_rules! make_tests {
        {
            $(
                $name:ident : $ty:ty => { $value:expr } == { $be:expr, $le:expr };
            )*
        } => {
            $(
                #[test]
                fn $name() {
                    let got = <$ty as Encode::<BigEndian>>::encode(&$value);
                    if got != *$be {
                        panic!(
                            r#"encoding big endian:

expected:
{}

got:
{}"#,
                            Hexdump::new($be),
                            Hexdump::new(&got),
                        )
                    }

                    let got = <$ty as Encode::<LittleEndian>>::encode(&$value);
                    if got != *$le {
                        panic!(
                            r#"encoding little endian:

expected:
{}

got:
{}"#,
                            Hexdump::new($le),
                            Hexdump::new(&got),
                        )
                    }

                    let got = <$ty as Decode::<BigEndian>>::decode($be);
                    let expected: $ty = $value;
                    if got != expected {
                        panic!(
                            r#"decoding big endian:
expected: {:?}
got:      {:?}"#,
                            expected,
                            got,
                        )
                    }

                    let got = <$ty as Decode::<LittleEndian>>::decode($le);
                    let expected: $ty = $value;
                    if got != expected {
                        panic!(
                            r#"decoding little endian:
expected: {:?}
got:      {:?}"#,
                            expected,
                            got,
                        )
                    }
                }
            )*
        };
    }

    make_tests! {
        test_u16 : u16 => { 0x1234 } == { b"\x12\x34", b"\x34\x12" };
        test_i16 : i16 => { 0x1234 } == { b"\x12\x34", b"\x34\x12" };

        test_u32 : u32 => { 0x12345678 } == { b"\x12\x34\x56\x78", b"\x78\x56\x34\x12" };
        test_i32 : i32 => { 0x12345678 } == { b"\x12\x34\x56\x78", b"\x78\x56\x34\x12" };

        test_u64 : u64 => { 0x123456789abcdef0 } == {
            b"\x12\x34\x56\x78\x9a\xbc\xde\xf0",
            b"\xf0\xde\xbc\x9a\x78\x56\x34\x12"
        };
        test_i64 : i64 => { 0x123456789abcdef0 } == {
            b"\x12\x34\x56\x78\x9a\xbc\xde\xf0",
            b"\xf0\xde\xbc\x9a\x78\x56\x34\x12"
        };

        test_u128 : u128 => { 0x123456789abcdef00fedcba987654321 } == {
            b"\x12\x34\x56\x78\x9a\xbc\xde\xf0\x0f\xed\xcb\xa9\x87\x65\x43\x21",
            b"\x21\x43\x65\x87\xa9\xcb\xed\x0f\xf0\xde\xbc\x9a\x78\x56\x34\x12"
        };
        test_i128 : i128 => { 0x123456789abcdef00fedcba987654321 } == {
            b"\x12\x34\x56\x78\x9a\xbc\xde\xf0\x0f\xed\xcb\xa9\x87\x65\x43\x21",
            b"\x21\x43\x65\x87\xa9\xcb\xed\x0f\xf0\xde\xbc\x9a\x78\x56\x34\x12"
        };
    }
}
