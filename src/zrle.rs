use std;
use std::io::Read;
use flate2;
use byteorder::ReadBytesExt;
use ::{protocol, Error, Result, Rect};

struct ZlibReader<'a> {
    decompressor: flate2::Decompress,
    input:        &'a [u8]
}

impl<'a> ZlibReader<'a> {
    fn new(decompressor: flate2::Decompress, input: &'a [u8]) -> ZlibReader<'a> {
        ZlibReader { decompressor: decompressor, input: input }
    }

    fn into_inner(self) -> Result<flate2::Decompress> {
        if self.input.len() == 0 {
            Ok(self.decompressor)
        } else {
            Err(Error::Unexpected("leftover ZRLE byte data"))
        }
    }
}

impl<'a> Read for ZlibReader<'a> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let in_before  = self.decompressor.total_in();
        let out_before = self.decompressor.total_out();
        let result = self.decompressor.decompress(self.input, output, flate2::Flush::None);
        let consumed = (self.decompressor.total_in()  - in_before) as usize;
        let produced = (self.decompressor.total_out() - out_before) as usize;

        self.input = &self.input[consumed..];
        match result {
            Ok(flate2::Status::Ok) => Ok(produced),
            Ok(flate2::Status::BufError) => Ok(0),
            Err(error) =>
                Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            Ok(flate2::Status::StreamEnd) =>
                Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "ZRLE stream end"))
        }
    }
}

struct BitReader<T: Read> {
    reader:   T,
    buffer:   u8,
    position: usize
}

impl<T: Read> BitReader<T> {
    fn new(reader: T) -> BitReader<T> {
        BitReader { reader: reader, buffer: 0, position: 8 }
    }

    fn into_inner(self) -> Result<T> {
        if self.position == 8 {
            Ok(self.reader)
        } else {
            Err(Error::Unexpected("leftover ZRLE bit data"))
        }
    }

    fn read_bits(&mut self, count: usize) -> std::io::Result<u8> {
        assert!(count > 0 && count <= 8);

        if self.position == 8 {
            self.buffer = try!(self.reader.read_u8());
            self.position = 0;
        }

        if self.position + count <= 8 {
            let shift = 8 - (count + self.position);
            let mask  = (1 << count) - 1;
            let result = (self.buffer >> shift) & mask;
            self.position += count;
            Ok(result)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                                    "unaligned ZRLE bit read"))
        }
    }

    fn read_bit(&mut self) -> std::io::Result<bool> {
        Ok(try!(self.read_bits(1)) != 0)
    }

    fn align(&mut self) {
        self.position = 8;
    }
}

impl<T: Read> Read for BitReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position == 8 {
            self.reader.read(buf)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                                    "unaligned ZRLE byte read"))
        }
    }
}

pub struct Decoder {
    decompressor: Option<flate2::Decompress>
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder { decompressor: Some(flate2::Decompress::new(/*zlib_header*/true)) }
    }

    pub fn decode<F>(&mut self, format: protocol::PixelFormat, rect: Rect,
                 input: &[u8], mut callback: F) -> Result<bool>
            where F: FnMut(Rect, Vec<u8>) -> Result<bool> {
        fn read_pixel(reader: &mut Read, pad: bool, bpp: usize) -> Result<[u8; 4]> {
            let mut entry = [0; 4];
            try!(reader.read_exact(&mut entry[if pad { 1 } else { 0 }..bpp]));
            Ok(entry)
        }

        fn read_run_length(reader: &mut Read) -> Result<usize> {
            let mut run_length_part = try!(reader.read_u8());
            let mut run_length = 1 + run_length_part as usize;
            while run_length_part == 255 {
                run_length_part = try!(reader.read_u8());
                run_length += run_length_part as usize;
            }
            Ok(run_length)
        }

        let bpp = format.bits_per_pixel as usize / 8;
        let pixel_mask =
            (format.red_max   as u32) << format.red_shift   |
            (format.green_max as u32) << format.green_shift |
            (format.blue_max  as u32) << format.blue_shift;
        let (compressed_bpp, pad_pixel) =
            if format.bits_per_pixel == 32 && format.true_colour == true && format.depth <= 24 {
                if pixel_mask & 0x000000ff == 0 {
                    (3, !format.big_endian)
                } else if pixel_mask & 0xff000000 == 0 {
                    (3, format.big_endian)
                } else {
                    (4, false)
                }
            } else {
                (format.bits_per_pixel as usize / 4, false)
            };

        let mut reader = BitReader::new(ZlibReader::new(self.decompressor.take().unwrap(), input));

        let mut y = 0;
        while y < rect.height {
            let height = if y + 64 > rect.height { rect.height - y } else { 64 };
            let mut x = 0;
            while x < rect.width {
                let width = if x + 64 > rect.width { rect.width - x } else { 64 };

                let is_rle = try!(reader.read_bit());
                let palette_size = try!(reader.read_bits(7));

                let mut palette = Vec::<[u8; 4]>::new();
                for _ in 0..palette_size {
                    palette.push(try!(read_pixel(&mut reader, pad_pixel, compressed_bpp)))
                }

                let mut pixels = Vec::new();
                match (is_rle, palette_size) {
                    (false, 0) => { // Raw pixels
                        for _ in 0..width * height {
                            let pixel = try!(read_pixel(&mut reader, pad_pixel, compressed_bpp));
                            pixels.extend_from_slice(&pixel[0..bpp]);
                        }
                    },
                    (false, 1) => { // Color fill
                        for _ in 0..width * height {
                            pixels.extend_from_slice(&palette[0][0..bpp]);
                        }
                    },
                    (false, 2) | (false, 3...4) | (false, 5...16) => { // Indexed pixels
                        let bits_per_index =
                            match palette_size {
                                2 => 1, 3...4 => 2, 5...16 => 4, _ => unreachable!()
                            };
                        for _ in 0..height {
                            for _ in 0..width {
                                let index = try!(reader.read_bits(bits_per_index));
                                pixels.extend_from_slice(&palette[index as usize][0..bpp])
                            }
                            reader.align();
                        }
                    },
                    (true, 0) => { // Raw RLE
                        let mut count = 0;
                        while count < (width * height) as usize {
                            let pixel = try!(read_pixel(&mut reader, pad_pixel, compressed_bpp));
                            let run_length = try!(read_run_length(&mut reader));
                            for _ in 0..run_length {
                                pixels.extend_from_slice(&pixel[0..bpp]);
                            }
                            count += run_length;
                        }
                    },
                    (true, 2...127) => { // Indexed RLE
                        let mut count = 0;
                        while count < (width * height) as usize {
                            let longer_than_one = try!(reader.read_bit());
                            let index = try!(reader.read_bits(7));
                            let run_length =
                                if longer_than_one {
                                    try!(read_run_length(&mut reader))
                                } else {
                                    1
                                };
                            for _ in 0..run_length {
                                pixels.extend_from_slice(&palette[index as usize][0..bpp]);
                            }
                            count += run_length;
                        }
                    },
                    _ => return Err(Error::Unexpected("ZRLE subencoding"))
                }

                let tile = Rect { top: rect.top + y, left: rect.left + x,
                                  width: width, height: height };
                if let false = try!(callback(tile, pixels)) {
                    return Ok(false)
                }

                x += width;
            }
            y += height;
        }

        self.decompressor = Some(try!(try!(reader.into_inner()).into_inner()));
        Ok(true)
    }
}