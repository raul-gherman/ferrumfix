//! Simple Open Framing Header (SOFH) support.
//!
//! SOFH provides encoding-agnostic message framing. By SOFH rules, each payload
//! is preceded by a header that consists of six (6) bytes, which contain
//! information regarding both
//! - payload's encoding type
//! - payload's total length
//!
//! Please refer to https://www.fixtrading.org/standards/fix-sofh/ for more
//! information.

use super::Poll;
use super::{Decoder, Encoder, FramelessDecoder};
use crate::utils::Buffer;
use std::convert::TryInto;
use std::fmt;
use std::io;

const HEADER_LENGTH: usize = 6;

/// A parser for Simple Open Framing Header (SOFH) -encoded messages.
#[derive(Debug)]
pub struct BufCodec {
    buffer: Vec<u8>,
    header: Option<(usize, u16)>,
}

impl BufCodec {
    /// Creates a new SOFH parser with default buffer size.
    pub fn new() -> Self {
        Self::with_capacity(1024)
    }

    /// Creates a new [`Codec`](Codec) with a buffer large enough to
    /// hold `capacity` amounts of bytes without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            header: None,
        }
    }

    /// Returns the current buffer capacity of this [`Codec`]. This value is
    /// subject to change after every incoming message.
    ///
    /// # Examples
    ///
    /// ```
    /// use fasters::codec::sofh::Codec;
    ///
    /// let parser = Codec::with_capacity(8192);
    /// assert_eq!(parser.capacity(), 8192);
    /// ```
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }
}

impl<'a> FramelessDecoder<'a, Frame<'a>> for BufCodec {
    type Error = DecodeError;

    fn supply_buffer(&mut self) -> &mut [u8] {
        let buffer_len = self.buffer.len();
        let additional_capacity = match self.header {
            None => 6,
            Some((len, _)) => (len as i64 - buffer_len as i64).max(0),
        };
        for _ in 0..additional_capacity {
            self.buffer.push(0);
        }
        &mut self.buffer[buffer_len..]
    }

    fn attempt_decoding(&mut self) -> Result<Poll, Self::Error> {
        match self.header {
            None => {
                if self.buffer.len() >= 6 {
                    self.header = Some((
                        get_message_length(&self.buffer[..]) as usize,
                        get_encoding_type(&self.buffer[..]),
                    ));
                }
                Ok(Poll::Incomplete)
            }
            Some((len, _)) if len < HEADER_LENGTH => {
                Err(DecodeError::InvalidMessageLength(len.try_into().unwrap()))
            }
            Some((len, _)) if len < self.buffer.len() => Ok(Poll::Incomplete),
            Some((_, _)) => Ok(Poll::Ready),
        }
    }

    fn get_item(&'a self) -> Frame<'a> {
        Frame::new(self.header.unwrap().1, &self.buffer[..])
    }
}

fn get_message_length(data: &[u8]) -> u32 {
    u32::from_be_bytes(data[0..4].try_into().unwrap())
}

fn get_encoding_type(data: &[u8]) -> u16 {
    u16::from_be_bytes(data[4..HEADER_LENGTH].try_into().unwrap())
}

#[derive(Debug, Clone, Default)]
pub struct Codec;

impl<'a> Decoder<'a, Frame<'a>> for Codec {
    type Error = DecodeError;

    fn decode(&mut self, data: &'a [u8]) -> Result<Frame<'a>, Self::Error> {
        let err = || DecodeError::InvalidMessageLength(data.len() as u32);
        if data.len() < HEADER_LENGTH {
            return Err(err());
        }
        // Note that the message length field also includes the header.
        if data.len() != get_message_length(data) as usize {
            return Err(err());
        }
        let encoding_type = get_encoding_type(data);
        Ok(Frame::new(encoding_type, &data[HEADER_LENGTH..]))
    }
}

impl<'a> Encoder<Frame<'a>> for Codec {
    type Error = EncodeError;

    fn encode(
        &mut self,
        mut buffer: impl Buffer,
        message: &Frame<'a>,
    ) -> std::result::Result<usize, Self::Error> {
        let len = message.payload().len();
        let body_len: u32 = len.try_into().map_err(|_| Self::Error::TooLong(len))?;
        let message_length = body_len.to_be_bytes();
        let encoding_type = message.encoding_type().to_be_bytes();
        buffer.extend_from_slice(&message_length[..]);
        buffer.extend_from_slice(&encoding_type[..]);
        buffer.extend_from_slice(message.payload());
        Ok(buffer.len())
    }
}

#[derive(Debug, Clone)]
pub enum EncodeError {
    TooLong(usize),
}

/// A non-owning message frame, with an internal pointer to the buffer that
/// contains the raw data.
#[derive(Debug, Clone)]
pub struct Frame<'a> {
    encoding_type: u16,
    payload: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Creates a new [`Frame`] with `payload` as its contents and tagged with
    /// `encoding_type`.
    pub fn new(encoding_type: u16, payload: &'a [u8]) -> Self {
        Self {
            encoding_type,
            payload,
        }
    }

    /// Returns the encoding type for this message.
    pub fn encoding_type(&self) -> u16 {
        self.encoding_type
    }

    /// Returns an immutable reference to the internal buffer that contains the
    /// message payload.
    pub fn payload(&self) -> &[u8] {
        self.payload
    }
}

/// The error type that can be returned if some error occurs during SOFH parsing.
#[derive(Debug)]
pub enum DecodeError {
    InvalidMessageLength(u32),
    Io(io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(err) => {
                writeln!(f, "I/O error while reading the message.")?;
                (*err).fmt(f)
            }
            DecodeError::InvalidMessageLength(len) => {
                writeln!(
                    f,
                    "Message length is {} but it must be greater than or equal to 6.",
                    len
                )
            }
        }
    }
}

impl From<io::Error> for DecodeError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Enumeration type mapped from the 16-bit raw space.
///
/// One should always prefer to deal with raw 16-bit values and only convert to
/// [`EncodingType`] when matching.
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub enum EncodingType {
    /// User-specified encoding type. Legal values and their respective semantics
    /// ought to be agreed upon out-of-band by counterparties.
    ///
    /// Please note that `0x0` is *not* a valid [`EncodingType::Private`] value.
    Private(u8),
    /// Simple Binary Encoding (SBE) v1.0, big-endian mode.
    /// Please refer to https://www.fixtrading.org/standards/sbe/ for more
    /// information.
    SimpleBinaryEncodingV10BE,
    /// Simple Binary Encoding (SBE) v1.0, little-endian mode.
    /// Please refer to https://www.fixtrading.org/standards/sbe/ for more
    /// information.
    SimpleBinaryEncodingV10LE,
    /// Google's "Protobuf".
    /// Please refer to https://www.fixtrading.org/standards/gpb/ for more
    /// information.
    Protobuf,
    /// ASN.1 with Packed Encoding Rules (PER).
    /// Please refer to https://www.fixtrading.org/standards/asn1/ for more
    /// information.
    Asn1PER,
    /// ASN.1 with Basic Encoding Rules (BER).
    /// Please refer to https://www.fixtrading.org/standards/asn1/ for more
    /// information.
    Asn1BER,
    /// ASN.1 with Octet Encoding Rules (OER).
    /// Please refer to https://www.fixtrading.org/standards/asn1/ for more
    /// information.
    Asn1OER,
    /// Tag-value (classic) encoding.
    /// Please refer to https://www.fixtrading.org/standards/tagvalue/ for more
    /// information.
    TagValue,
    /// FIXML encoding.
    /// Please refer to https://www.fixtrading.org/standards/fixml/ for more
    /// information.
    FixmlSchema,
    /// FAST encoding.
    /// Please refer to https://www.fixtrading.org/standards/fast/ for more
    /// information.
    ///
    /// Please note that `0xFA00` is *not* a valid [`EncodingType::Fast`] value.
    Fast(u8),
    /// JSON encoding.
    /// Please refer to https://www.fixtrading.org/standards/json/ for more
    /// information.
    Json,
    /// BSON encoding.
    /// Please refer to https://www.fixtrading.org/standards/bson/ for more
    /// information.
    Bson,
    /// Unknown value.
    Unknown(u16),
}

impl From<u16> for EncodingType {
    fn from(encoding_type: u16) -> Self {
        // https://www.fixtrading.org/standards/fix-sofh-online/#encoding_type-field
        match encoding_type {
            0x1..=0xFF => EncodingType::Private(encoding_type as u8),
            0x4700 => EncodingType::Protobuf,
            0x5BE0 => EncodingType::SimpleBinaryEncodingV10BE,
            0xA500 => EncodingType::Asn1PER,
            0xA501 => EncodingType::Asn1BER,
            0xA502 => EncodingType::Asn1OER,
            0xEB50 => EncodingType::SimpleBinaryEncodingV10LE,
            0xF000 => EncodingType::TagValue,
            0xF100 => EncodingType::FixmlSchema,
            0xF500 => EncodingType::Json,
            0xFA01..=0xFAFF => EncodingType::Fast((encoding_type - 0xFA00) as u8),
            0xFB00 => EncodingType::Bson,
            _ => EncodingType::Unknown(encoding_type),
        }
    }
}

impl From<EncodingType> for u16 {
    fn from(encoding_type: EncodingType) -> Self {
        match encoding_type {
            EncodingType::Private(x) => x as u16,
            EncodingType::Protobuf => 0x4700,
            EncodingType::SimpleBinaryEncodingV10BE => 0x5BE0,
            EncodingType::Asn1PER => 0xA500,
            EncodingType::Asn1BER => 0xA501,
            EncodingType::Asn1OER => 0xA502,
            EncodingType::SimpleBinaryEncodingV10LE => 0xEB50,
            EncodingType::TagValue => 0xF000,
            EncodingType::FixmlSchema => 0xF100,
            EncodingType::Json => 0xF500,
            EncodingType::Fast(x) => 0xFA00u16 + (x as u16),
            EncodingType::Bson => 0xFB00,
            EncodingType::Unknown(x) => x,
        }
    }
}

impl PartialEq for EncodingType {
    fn eq(&self, other: &Self) -> bool {
        u16::from(*self) == u16::from(*other)
    }
}

impl std::cmp::Eq for EncodingType {}

#[cfg(test)]
mod test {
    use super::*;
    use crate::codec::FramelessError;
    use crate::StreamIterator;

    fn _frames_with_increasing_length() -> impl Iterator<Item = Vec<u8>> {
        std::iter::once(()).enumerate().map(|(i, ())| {
            let header = encode_header(i as u32 + 6, 0);
            let mut buffer = Vec::new();
            buffer.extend_from_slice(&header[..]);
            for _ in 0..i {
                buffer.extend_from_slice(&[0]);
            }
            buffer
        })
    }

    struct Reader<T> {
        source: T,
    }

    impl<T> std::io::Read for Reader<T>
    where
        T: Iterator<Item = u8>,
    {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            for i in 0..buffer.len() {
                buffer[i] = self.source.next().unwrap();
            }
            Ok(buffer.len())
        }
    }

    fn _increasing_frames_as_read() -> impl std::io::Read {
        let stream = _frames_with_increasing_length()
            .map(|vec| vec.into_iter())
            .flatten();
        Reader { source: stream }
    }

    fn encode_header(len: u32, encoding_type: u16) -> [u8; 6] {
        let a = len.to_be_bytes();
        let b = encoding_type.to_be_bytes();
        let mut bytes = [0u8; 6];
        bytes[0..4].copy_from_slice(&a);
        bytes[4..6].copy_from_slice(&b);
        bytes
    }

    #[test]
    fn encoding_type_conversion_is_correct() {
        let mut value = 0u16;
        loop {
            let encoding_type = EncodingType::from(value);
            assert_eq!(value, u16::from(encoding_type));
            if value == u16::MAX {
                return;
            }
            value += 1;
        }
    }

    #[test]
    fn low_values_correspond_to_private_encoding_types() {
        for value in &[0x1, 0x82, 0xff] {
            let encoding_type = EncodingType::from(*value);
            match encoding_type {
                EncodingType::Private(x) if x as u16 == *value => (),
                _ => panic!(),
            };
        }
    }

    #[test]
    fn every_encoding_type_is_equal_to_itself() {
        let mut value = 0u16;
        loop {
            let encoding_type = EncodingType::from(value);
            assert_eq!(encoding_type, encoding_type);
            if value == u16::MAX {
                return;
            }
            value += 1;
        }
    }

    #[test]
    fn value_0x100u16_is_not_a_private_encoding_type() {
        let encoding_type = EncodingType::from(0x100);
        if let EncodingType::Private(_) = encoding_type {
            panic!();
        }
    }

    #[test]
    fn frameless_decoder_returns_error_when_frame_has_len_lt_6() {
        for len in 0..6 {
            let header = encode_header(len, 0x4324);
            let parser = BufCodec::new();
            let mut frames = parser.frames_streamiter(&header[..]);
            let frame = frames.next();
            match frame {
                Some(Err(FramelessError::Decoder(DecodeError::InvalidMessageLength(_)))) => (),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn decoder_returns_error_when_frame_has_len_lt_6() {
        for len in 0..6 {
            let header = encode_header(len, 0x4324);
            let mut parser = Codec::default();
            let frame = parser.decode(&header[..]);
            match frame {
                Err(DecodeError::InvalidMessageLength(_)) => (),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn decoder_accepts_frame_with_len_6() {
        let header = encode_header(6, 0x4324);
        let mut parser = Codec::default();
        let frame = parser.decode(&header[..]);
        if frame.is_err() {
            panic!();
        }
    }
}
