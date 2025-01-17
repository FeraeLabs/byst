use super::r#impl::BytesImpl;
use crate::{
    buf::Length,
    io::{
        BufReader,
        End,
    },
    Buf,
    Range,
    RangeOutOfBounds,
};

#[derive(Clone, Copy)]
pub struct Static(pub &'static [u8]);

impl Length for Static {
    #[inline]
    fn len(&self) -> usize {
        self.0.len()
    }
}

impl<'b> BytesImpl<'b> for Static {
    #[inline]
    fn clone(&self) -> Box<dyn BytesImpl<'b> + 'b> {
        Box::new(*self)
    }

    fn peek_chunk(&self) -> Option<&[u8]> {
        BufReader::peek_chunk(&self.0)
    }

    #[inline]
    fn view(&self, range: Range) -> Result<Box<dyn BytesImpl<'b> + 'b>, RangeOutOfBounds> {
        Ok(Box::new(Buf::view(&self.0, range)?))
    }

    fn advance(&mut self, by: usize) -> Result<(), End> {
        BufReader::advance(&mut self.0, by)
    }
}
