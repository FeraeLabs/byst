use std::fmt::Debug;

use super::{
    r#impl::BytesImpl,
    r#static::Static,
    view::View,
};
use crate::{
    buf::{
        Empty,
        Length,
    },
    impl_me,
    io::{
        BufReader,
        End,
        Seek,
    },
    util::{
        buf_eq,
        cfg_pub,
        debug_as_hexdump,
    },
    Buf,
    Range,
    RangeOutOfBounds,
};

#[derive(Clone)]
pub struct Bytes {
    inner: View<'static>,
}

impl Bytes {
    /// Creates an empty [`Bytes`].
    ///
    /// This doesn't allocate.
    #[inline]
    pub fn new() -> Self {
        // note: this really doesn't allocate, since [`Empty`] is a ZST, and a `dyn ZST`
        // is ZST itself.[1]
        //
        // [1]: https://users.rust-lang.org/t/what-does-box-dyn-actually-allocate/56618/2
        Self::from_impl(Box::new(Empty))
    }

    cfg_pub! {
        #[inline]
        pub(#[cfg(feature = "bytes-impl")]) fn from_impl(inner: Box<dyn BytesImpl<'static> + 'static>) -> Self {
            View::from_impl(inner).into()
        }
    }
}

impl From<View<'static>> for Bytes {
    #[inline]
    fn from(inner: View<'static>) -> Self {
        Self { inner }
    }
}

impl Default for Bytes {
    /// Creates an empty [`Bytes`].
    ///
    /// This doesn't allocate.
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for Bytes {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_as_hexdump(f, self)
    }
}

impl<R: Buf> PartialEq<R> for Bytes {
    #[inline]
    fn eq(&self, other: &R) -> bool {
        buf_eq(self, other)
    }
}

impl From<&'static [u8]> for Bytes {
    #[inline]
    fn from(value: &'static [u8]) -> Self {
        Self::from_impl(Box::new(Static(value)))
    }
}

impl Buf for Bytes {
    type View<'a> = Self
    where
        Self: 'a;

    type Reader<'a> = Self
    where
        Self: 'a;

    #[inline]
    fn view(&self, range: impl Into<Range>) -> Result<Self::View<'_>, RangeOutOfBounds> {
        Ok(Buf::view(&self.inner, range.into())?.into())
    }

    #[inline]
    fn reader(&self) -> Self::Reader<'_> {
        self.clone()
    }
}

impl BufReader for Bytes {
    type View = Self;

    #[inline]
    fn peek_chunk(&self) -> Option<&[u8]> {
        <View as BufReader>::peek_chunk(&self.inner)
    }

    #[inline]
    fn view(&mut self, length: usize) -> Result<Self::View, End> {
        Ok(Bytes::from(<View as BufReader>::view(
            &mut self.inner,
            length,
        )?))
    }

    #[inline]
    fn peek_view(&self, length: usize) -> Result<Self::View, End> {
        Ok(Bytes::from(<View as BufReader>::peek_view(
            &self.inner,
            length,
        )?))
    }

    #[inline]
    fn rest(&mut self) -> Self::View {
        Bytes::from(<View as BufReader>::rest(&mut self.inner))
    }

    #[inline]
    fn peek_rest(&self) -> Self::View {
        Bytes::from(<View as BufReader>::peek_rest(&self.inner))
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), End> {
        <View as BufReader>::advance(&mut self.inner, by)
    }

    #[inline]
    fn remaining(&self) -> usize {
        <View as BufReader>::remaining(&self.inner)
    }
}

impl Seek for Bytes {
    type Position = Self;

    #[inline]
    fn tell(&self) -> Self::Position {
        Bytes::from(self.inner.tell())
    }

    #[inline]
    fn seek(&mut self, position: &Self::Position) -> Self::Position {
        Bytes::from(self.inner.seek(&position.inner))
    }
}

impl Length for Bytes {
    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl_me! {
    impl Reader for Bytes as BufReader;
    impl Read<_, ()> for Bytes as BufReader::View;
}
