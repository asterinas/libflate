// https://tools.ietf.org/html/rfc1950
use std::io;
use byteorder::BigEndian;
use byteorder::ReadBytesExt;
use byteorder::WriteBytesExt;

use lz77;
use deflate;
use checksum;
use finish::Finish;

const COMPRESSION_METHOD_DEFLATE: u8 = 8;

#[derive(Debug, Clone, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum CompressionLevel {
    Fastest = 0,
    Fast = 1,
    Default = 2,
    Slowest = 3,
}
impl CompressionLevel {
    fn from_u2(level: u8) -> Self {
        match level {
            0 => CompressionLevel::Fastest,
            1 => CompressionLevel::Fast,
            2 => CompressionLevel::Default,
            3 => CompressionLevel::Slowest,
            _ => unreachable!(),
        }
    }
    fn as_u2(&self) -> u8 {
        self.clone() as u8
    }
}
impl From<lz77::CompressionLevel> for CompressionLevel {
    fn from(f: lz77::CompressionLevel) -> Self {
        match f {
            lz77::CompressionLevel::None => CompressionLevel::Fastest,
            lz77::CompressionLevel::Fast => CompressionLevel::Fast,
            lz77::CompressionLevel::Balance => CompressionLevel::Default,
            lz77::CompressionLevel::Best => CompressionLevel::Slowest,
        }
    }
}

#[derive(Debug, Clone, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum Lz77WindowSize {
    B256 = 0,
    B512 = 1,
    KB1 = 2,
    KB2 = 3,
    KB4 = 4,
    KB8 = 5,
    KB16 = 6,
    KB32 = 7,
}
impl Lz77WindowSize {
    fn from_u4(compression_info: u8) -> Option<Self> {
        match compression_info {
            0 => Some(Lz77WindowSize::B256),
            1 => Some(Lz77WindowSize::B512),
            2 => Some(Lz77WindowSize::KB1),
            3 => Some(Lz77WindowSize::KB2),
            4 => Some(Lz77WindowSize::KB4),
            5 => Some(Lz77WindowSize::KB8),
            6 => Some(Lz77WindowSize::KB16),
            7 => Some(Lz77WindowSize::KB32),
            _ => None,
        }
    }
    fn as_u4(&self) -> u8 {
        self.clone() as u8
    }
    pub fn from_u16(size: u16) -> Self {
        use self::Lz77WindowSize::*;
        if 16384 < size {
            KB32
        } else if 8192 < size {
            KB16
        } else if 4096 < size {
            KB8
        } else if 2048 < size {
            KB4
        } else if 1024 < size {
            KB2
        } else if 512 < size {
            KB1
        } else if 256 < size {
            B512
        } else {
            B256
        }
    }
    pub fn to_u16(&self) -> u16 {
        use self::Lz77WindowSize::*;
        match *self {
            B256 => 256,
            B512 => 512,
            KB1 => 1024,
            KB2 => 2048,
            KB4 => 4096,
            KB8 => 8192,
            KB16 => 16384,
            KB32 => 32768,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Header {
    window_size: Lz77WindowSize,
    compression_level: CompressionLevel,
}
impl Header {
    pub fn window_size(&self) -> Lz77WindowSize {
        self.window_size.clone()
    }
    pub fn compression_level(&self) -> CompressionLevel {
        self.compression_level.clone()
    }
    fn from_lz77<E>(lz77: &E) -> Self
        where E: lz77::Lz77Encode
    {
        Header {
            compression_level: From::from(lz77.compression_level()),
            window_size: Lz77WindowSize::from_u16(lz77.window_size()),
        }
    }
    fn read_from<R>(mut reader: R) -> io::Result<Self>
        where R: io::Read
    {
        let cmf = try!(reader.read_u8());
        let flg = try!(reader.read_u8());
        let check = ((cmf as u16) << 8) + flg as u16;
        if check % 31 != 0 {
            return Err(invalid_data_error!("Inconsistent ZLIB check bits: `CMF({}) * 256 + \
                                            FLG({})` must be a multiple of 31",
                                           cmf,
                                           flg));
        }

        let compression_method = cmf & 0b1111;
        let compression_info = cmf >> 4;
        if compression_method != COMPRESSION_METHOD_DEFLATE {
            return Err(invalid_data_error!("Compression methods other than DEFLATE(8) are \
                                            unsupported: method={}",
                                           compression_method));
        }
        let window_size = try!(Lz77WindowSize::from_u4(compression_info).ok_or_else(|| {
            invalid_data_error!("CINFO above 7 are not allowed: value={}", compression_info)
        }));

        let dict_flag = (flg & 0b100000) != 0;
        if dict_flag {
            let dictionary_id = try!(reader.read_u32::<BigEndian>());
            return Err(invalid_data_error!("Preset dictionaries are not supported: \
                                            dictionary_id=0x{:X}",
                                           dictionary_id));
        }
        let compression_level = CompressionLevel::from_u2(flg >> 6);
        Ok(Header {
            window_size: window_size,
            compression_level: compression_level,
        })
    }
    fn write_to<W>(&self, mut writer: W) -> io::Result<()>
        where W: io::Write
    {
        let cmf = (self.window_size.as_u4() << 4) | COMPRESSION_METHOD_DEFLATE;
        let mut flg = self.compression_level.as_u2() << 6;
        let check = ((cmf as u16) << 8) + flg as u16;
        if check % 31 != 0 {
            flg += (31 - check % 31) as u8;
        }
        try!(writer.write_u8(cmf));
        try!(writer.write_u8(flg));
        Ok(())
    }
}

#[derive(Debug)]
pub struct Decoder<R> {
    header: Header,
    reader: deflate::Decoder<R>,
    adler32: checksum::Adler32,
    eos: bool,
}
impl<R> Decoder<R>
    where R: io::Read
{
    pub fn new(mut inner: R) -> io::Result<Self> {
        let header = try!(Header::read_from(&mut inner));
        Ok(Decoder {
            header: header,
            reader: deflate::Decoder::new(inner),
            adler32: checksum::Adler32::new(),
            eos: false,
        })
    }
    pub fn header(&self) -> &Header {
        &self.header
    }
    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}
impl<R> io::Read for Decoder<R>
    where R: io::Read
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.eos {
            Ok(0)
        } else {
            let read_size = try!(self.reader.read(buf));
            if read_size == 0 {
                self.eos = true;
                let adler32 = try!(self.reader.as_inner_mut().read_u32::<BigEndian>());
                if adler32 != self.adler32.value() {
                    Err(invalid_data_error!("Adler32 checksum mismatched: value={}, expected={}",
                                            self.adler32.value(),
                                            adler32))
                } else {
                    Ok(0)
                }
            } else {
                self.adler32.update(&buf[..read_size]);
                Ok(read_size)
            }
        }
    }
}

#[derive(Debug)]
pub struct EncodeOptions<E>
    where E: lz77::Lz77Encode
{
    header: Header,
    options: deflate::EncodeOptions<E>,
}
impl Default for EncodeOptions<lz77::DefaultLz77Encoder> {
    fn default() -> Self {
        EncodeOptions {
            header: Header::from_lz77(&lz77::DefaultLz77Encoder::new()),
            options: Default::default(),
        }
    }
}
impl EncodeOptions<lz77::DefaultLz77Encoder> {
    pub fn new() -> Self {
        Self::default()
    }
}
impl<E> EncodeOptions<E>
    where E: lz77::Lz77Encode
{
    pub fn with_lz77(lz77: E) -> Self {
        EncodeOptions {
            header: Header::from_lz77(&lz77),
            options: deflate::EncodeOptions::with_lz77(lz77),
        }
    }
    pub fn no_compression(mut self) -> Self {
        self.options = self.options.no_compression();
        self.header.compression_level = CompressionLevel::Fastest;
        self
    }
    pub fn block_size(mut self, size: usize) -> Self {
        self.options = self.options.block_size(size);
        self
    }
    pub fn dynamic_huffman_codes(mut self) -> Self {
        self.options = self.options.dynamic_huffman_codes();
        self
    }
    pub fn fixed_huffman_codes(mut self) -> Self {
        self.options = self.options.fixed_huffman_codes();
        self
    }
}

#[derive(Debug)]
pub struct Encoder<W, E = lz77::DefaultLz77Encoder> {
    header: Header,
    writer: deflate::Encoder<W, E>,
    adler32: checksum::Adler32,
}
impl<W> Encoder<W, lz77::DefaultLz77Encoder>
    where W: io::Write
{
    pub fn new(inner: W) -> io::Result<Self> {
        Self::with_options(inner, EncodeOptions::default())
    }
}
impl<W, E> Encoder<W, E>
    where W: io::Write,
          E: lz77::Lz77Encode
{
    pub fn with_options(mut inner: W, options: EncodeOptions<E>) -> io::Result<Self> {
        try!(options.header.write_to(&mut inner));
        Ok(Encoder {
            header: options.header,
            writer: deflate::Encoder::with_options(inner, options.options),
            adler32: checksum::Adler32::new(),
        })
    }
    pub fn header(&self) -> &Header {
        &self.header
    }
    pub fn finish(self) -> Finish<W, io::Error> {
        let mut inner = finish_try!(self.writer.finish());
        match inner.write_u32::<BigEndian>(self.adler32.value())
            .and_then(|_| inner.flush()) {
            Ok(_) => Finish::new(inner, None),
            Err(e) => Finish::new(inner, Some(e)),
        }
    }
}
impl<W> io::Write for Encoder<W>
    where W: io::Write
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written_size = try!(self.writer.write(buf));
        self.adler32.update(&buf[..written_size]);
        Ok(written_size)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

pub fn decode_all(buf: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = Decoder::new(io::Cursor::new(buf)).unwrap();
    let mut buf = Vec::with_capacity(buf.len());
    try!(io::copy(&mut decoder, &mut buf));
    Ok(buf)
}

#[cfg(test)]
mod test {
    use std::io;
    use super::*;

    #[test]
    fn decode_works() {
        let encoded = [120, 156, 243, 72, 205, 201, 201, 87, 8, 207, 47, 202, 73, 81, 4, 0, 28,
                       73, 4, 62];
        let mut decoder = Decoder::new(io::Cursor::new(&encoded)).unwrap();
        assert_eq!(*decoder.header(),
                   Header {
                       window_size: Lz77WindowSize::KB32,
                       compression_level: CompressionLevel::Default,
                   });

        let mut buf = Vec::new();
        io::copy(&mut decoder, &mut buf).unwrap();

        let expected = b"Hello World!";
        assert_eq!(buf, expected);
    }

    #[test]
    fn default_encode_works() {
        let plain = b"Hello World! Hello ZLIB!!";
        let mut encoder = Encoder::new(Vec::new()).unwrap();
        io::copy(&mut &plain[..], &mut encoder).unwrap();
        let encoded = encoder.finish().into_result().unwrap();
        assert_eq!(decode_all(&encoded).unwrap(), plain);
    }

    #[test]
    fn best_speed_encode_works() {
        let plain = b"Hello World! Hello ZLIB!!";
        let mut encoder = Encoder::with_options(Vec::new(),
                                                EncodeOptions::default().fixed_huffman_codes())
            .unwrap();
        io::copy(&mut &plain[..], &mut encoder).unwrap();
        let encoded = encoder.finish().into_result().unwrap();
        assert_eq!(decode_all(&encoded).unwrap(), plain);
    }

    #[test]
    fn raw_encode_works() {
        let plain = b"Hello World!";
        let mut encoder = Encoder::with_options(Vec::new(), EncodeOptions::new().no_compression())
            .unwrap();
        io::copy(&mut &plain[..], &mut encoder).unwrap();
        let encoded = encoder.finish().into_result().unwrap();
        let expected = [120, 1, 1, 12, 0, 243, 255, 72, 101, 108, 108, 111, 32, 87, 111, 114, 108,
                        100, 33, 28, 73, 4, 62];
        assert_eq!(encoded, expected);
        assert_eq!(decode_all(&encoded).unwrap(), plain);
    }
}
