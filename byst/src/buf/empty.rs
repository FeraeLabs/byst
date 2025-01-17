use std::ops::{
    Deref,
    DerefMut,
};

use super::{
    Buf,
    BufReader,
    BufWriter,
    Full,
    Length,
    SizeLimit,
};
use crate::{
    bytes::r#impl::{
        BytesImpl,
        BytesMutImpl,
        WriterImpl,
    },
    impl_me,
    io::{
        End,
        Reader,
        Seek,
    },
    BufMut,
    Bytes,
    IndexOutOfBounds,
    Range,
    RangeOutOfBounds,
};

/// An empty buffer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Empty;

impl From<Empty> for Bytes {
    #[inline]
    fn from(value: Empty) -> Self {
        Self::from_impl(Box::new(value))
    }
}

impl Buf for Empty {
    type View<'a> = Self
    where
        Self: 'a;

    type Reader<'a> = Self
    where
        Self: 'a;

    #[inline]
    fn view(&self, range: impl Into<Range>) -> Result<Self, RangeOutOfBounds> {
        check_range(range.into())?;
        Ok(Self)
    }

    #[inline]
    fn reader(&self) -> Self {
        Self
    }
}

impl BufMut for Empty {
    type ViewMut<'a> = Self
    where
        Self: 'a;

    type Writer<'a> = Self
    where
        Self: 'a;

    #[inline]
    fn view_mut(&mut self, range: impl Into<Range>) -> Result<Self, RangeOutOfBounds> {
        check_range(range.into())?;
        Ok(Self)
    }

    #[inline]
    fn writer(&mut self) -> Self {
        Self
    }

    #[inline]
    fn reserve(&mut self, size: usize) -> Result<(), Full> {
        if size == 0 {
            Ok(())
        }
        else {
            Err(Full {
                required: size,
                capacity: 0,
            })
        }
    }

    #[inline]
    fn size_limit(&self) -> SizeLimit {
        SizeLimit::Exact(0)
    }
}

impl Length for Empty {
    #[inline]
    fn len(&self) -> usize {
        0
    }

    #[inline]
    fn is_empty(&self) -> bool {
        true
    }
}

impl BufReader for Empty {
    type View = Self;

    #[inline]
    fn peek_chunk(&self) -> Option<&'static [u8]> {
        None
    }

    #[inline]
    fn view(&mut self, length: usize) -> Result<Self::View, End> {
        check_length_read(length)?;
        Ok(Self)
    }

    #[inline]
    fn peek_view(&self, length: usize) -> Result<Self::View, End> {
        check_length_read(length)?;
        Ok(Self)
    }

    #[inline]
    fn rest(&mut self) -> Self::View {
        Self
    }

    #[inline]
    fn peek_rest(&self) -> Self::View {
        Self
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), End> {
        check_length_read(by)
    }

    #[inline]
    fn remaining(&self) -> usize {
        0
    }
}

impl Seek for Empty {
    type Position = Self;

    #[inline]
    fn tell(&self) -> Self::Position {
        Self
    }

    #[inline]
    fn seek(&mut self, _position: &Self::Position) -> Self::Position {
        Self
    }
}

impl BufWriter for Empty {
    type ViewMut<'a> = Empty where Self: 'a;

    #[inline]
    fn peek_chunk_mut(&mut self) -> Option<&mut [u8]> {
        None
    }

    fn view_mut(&mut self, length: usize) -> Result<Self::ViewMut<'_>, crate::io::Full> {
        check_length_write(length)?;
        Ok(Self)
    }

    #[inline]
    fn peek_view_mut(&mut self, length: usize) -> Result<Self::ViewMut<'_>, crate::io::Full> {
        check_length_write(length)?;
        Ok(Self)
    }

    #[inline]
    fn rest_mut(&mut self) -> Self::ViewMut<'_> {
        Self
    }

    #[inline]
    fn peek_rest_mut(&mut self) -> Self::ViewMut<'_> {
        Self
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), crate::io::Full> {
        check_length_write(by)
    }

    #[inline]
    fn remaining(&self) -> usize {
        0
    }

    #[inline]
    fn extend(&mut self, with: &[u8]) -> Result<(), crate::io::Full> {
        check_length_write(with.len())
    }
}

impl Reader for Empty {
    type Error = End;

    #[inline]
    fn read_into<D: BufMut>(
        &mut self,
        _dest: D,
        _limit: impl Into<Option<usize>>,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }

    fn read_into_exact<D: BufMut>(&mut self, _dest: D, length: usize) -> Result<(), Self::Error> {
        check_length_read(length)
    }

    #[inline]
    fn skip(&mut self, amount: usize) -> Result<(), Self::Error> {
        check_length_read(amount)
    }
}

impl<T: Buf> PartialEq<T> for Empty {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        other.is_empty()
    }
}

impl Deref for Empty {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        Default::default()
    }
}

impl DerefMut for Empty {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Default::default()
    }
}

impl AsRef<[u8]> for Empty {
    fn as_ref(&self) -> &[u8] {
        Default::default()
    }
}

impl AsMut<[u8]> for Empty {
    fn as_mut(&mut self) -> &mut [u8] {
        Default::default()
    }
}

impl<'b> BytesImpl<'b> for Empty {
    fn view(&self, range: Range) -> Result<Box<dyn BytesImpl<'b> + 'b>, RangeOutOfBounds> {
        check_range(range)?;
        Ok(Box::new(Self))
    }

    fn clone(&self) -> Box<dyn BytesImpl<'b> + 'b> {
        Box::new(Self)
    }

    fn peek_chunk(&self) -> Option<&'_ [u8]> {
        None
    }

    fn advance(&mut self, by: usize) -> Result<(), End> {
        BufReader::advance(self, by)
    }
}

impl BytesMutImpl for Empty {
    fn view(&self, range: Range) -> Result<Box<dyn BytesImpl<'_> + '_>, RangeOutOfBounds> {
        check_range(range)?;
        Ok(Box::new(Self))
    }

    fn view_mut(
        &mut self,
        range: Range,
    ) -> Result<Box<dyn BytesMutImpl + 'static>, RangeOutOfBounds> {
        check_range(range)?;
        Ok(Box::new(Self))
    }

    fn reader(&self) -> Box<dyn BytesImpl<'_> + '_> {
        Box::new(Self)
    }

    fn writer(&mut self) -> Box<dyn WriterImpl> {
        Box::new(Self)
    }

    fn reserve(&mut self, size: usize) -> Result<(), Full> {
        BufMut::reserve(self, size)
    }

    fn size_limit(&self) -> SizeLimit {
        BufMut::size_limit(self)
    }

    fn split_at(&mut self, at: usize) -> Result<Box<dyn BytesMutImpl + '_>, IndexOutOfBounds> {
        if at == 0 {
            Ok(Box::new(Self))
        }
        else {
            Err(IndexOutOfBounds {
                required: at,
                bounds: (0, 0),
            })
        }
    }
}

impl WriterImpl for Empty {
    fn peek_chunk_mut(&mut self) -> Option<&mut [u8]> {
        None
    }

    fn advance(&mut self, by: usize) -> Result<(), crate::io::Full> {
        BufWriter::advance(self, by)
    }

    fn remaining(&self) -> usize {
        0
    }

    fn extend(&mut self, with: &[u8]) -> Result<(), crate::io::Full> {
        BufWriter::extend(self, with)
    }
}

impl_me! {
    impl Writer for Empty as BufWriter;
}

#[inline]
fn check_range(range: Range) -> Result<(), RangeOutOfBounds> {
    if range.start.unwrap_or_default() == 0 && range.end.unwrap_or_default() == 0 {
        Ok(())
    }
    else {
        Err(RangeOutOfBounds {
            required: range,
            bounds: (0, 0),
        })
    }
}

#[inline]
fn check_length_write(length: usize) -> Result<(), crate::io::Full> {
    if length == 0 {
        Ok(())
    }
    else {
        Err(crate::io::Full {
            written: 0,
            requested: length,
            remaining: 0,
        })
    }
}

#[inline]
fn check_length_read(length: usize) -> Result<(), End> {
    if length == 0 {
        Ok(())
    }
    else {
        Err(End {
            read: 0,
            requested: length,
            remaining: 0,
        })
    }
}
