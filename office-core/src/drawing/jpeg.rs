//! Minimal Word-compatible baseline JPEG writer.
//!
//! `jpeg-encoder` supplies its pure-Rust integer FDCT. Its stock RGB sampling
//! and reciprocal quantizer do not meet the real-image parity gate, so the
//! small amount of policy-sensitive glue stays local: libjpeg RGB conversion,
//! h2v2 box sampling, direct integer quantization, and baseline entropy coding.

use crate::{OfficeError, Result, UnsupportedCode};

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// Annex K tables scaled with libjpeg's quality-75 rule.
const LUMA_Q75: [u8; 64] = [
    8, 6, 5, 8, 12, 20, 26, 31, 6, 6, 7, 10, 13, 29, 30, 28, 7, 7, 8, 12, 20, 29, 35, 28, 7, 9, 11,
    15, 26, 44, 40, 31, 9, 11, 19, 28, 34, 55, 52, 39, 12, 18, 28, 32, 41, 52, 57, 46, 25, 32, 39,
    44, 52, 61, 60, 51, 36, 46, 48, 49, 56, 50, 52, 50,
];
const CHROMA_Q75: [u8; 64] = [
    9, 9, 12, 24, 50, 50, 50, 50, 9, 11, 13, 33, 50, 50, 50, 50, 12, 13, 28, 50, 50, 50, 50, 50,
    24, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
];

const LUMA_DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const LUMA_DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const CHROMA_DC_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const CHROMA_DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
const LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
const CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

pub(super) fn encode_word_quality_75_420(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3));
    if width == 0 || height == 0 || expected != Some(rgb.len()) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ImageDecodeFailure,
            "RGB image buffer has invalid dimensions",
        ));
    }
    let width16 = u16::try_from(width).map_err(|_| jpeg_dimension_error())?;
    let height16 = u16::try_from(height).map_err(|_| jpeg_dimension_error())?;

    let mut output = Vec::with_capacity(rgb.len() / 4);
    output.extend_from_slice(&[0xff, 0xd8]);
    write_segment(&mut output, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");

    let mut dqt = Vec::with_capacity(130);
    dqt.push(0);
    dqt.extend(ZIGZAG.iter().map(|&index| LUMA_Q75[index]));
    dqt.push(1);
    dqt.extend(ZIGZAG.iter().map(|&index| CHROMA_Q75[index]));
    write_segment(&mut output, 0xdb, &dqt);

    let mut sof = vec![8];
    sof.extend_from_slice(&height16.to_be_bytes());
    sof.extend_from_slice(&width16.to_be_bytes());
    sof.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    write_segment(&mut output, 0xc0, &sof);

    write_huffman_segment(&mut output, 0x00, &LUMA_DC_BITS, &LUMA_DC_VALUES);
    write_huffman_segment(&mut output, 0x10, &LUMA_AC_BITS, &LUMA_AC_VALUES);
    write_huffman_segment(&mut output, 0x01, &CHROMA_DC_BITS, &CHROMA_DC_VALUES);
    write_huffman_segment(&mut output, 0x11, &CHROMA_AC_BITS, &CHROMA_AC_VALUES);
    write_segment(&mut output, 0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let luma_dc = HuffmanTable::new(&LUMA_DC_BITS, &LUMA_DC_VALUES);
    let luma_ac = HuffmanTable::new(&LUMA_AC_BITS, &LUMA_AC_VALUES);
    let chroma_dc = HuffmanTable::new(&CHROMA_DC_BITS, &CHROMA_DC_VALUES);
    let chroma_ac = HuffmanTable::new(&CHROMA_AC_BITS, &CHROMA_AC_VALUES);
    let mut bits = BitWriter::new(&mut output);
    let mut previous_dc = [0i16; 3];

    for mcu_y in 0..height.div_ceil(16) {
        for mcu_x in 0..width.div_ceil(16) {
            for block_y in 0..2 {
                for block_x in 0..2 {
                    let block = luma_block(
                        rgb,
                        width,
                        height,
                        mcu_x * 16 + block_x * 8,
                        mcu_y * 16 + block_y * 8,
                    );
                    write_block(
                        &mut bits,
                        block,
                        &LUMA_Q75,
                        &mut previous_dc[0],
                        &luma_dc,
                        &luma_ac,
                    );
                }
            }
            let (cb, cr) = chroma_blocks(rgb, width, height, mcu_x * 16, mcu_y * 16);
            write_block(
                &mut bits,
                cb,
                &CHROMA_Q75,
                &mut previous_dc[1],
                &chroma_dc,
                &chroma_ac,
            );
            write_block(
                &mut bits,
                cr,
                &CHROMA_Q75,
                &mut previous_dc[2],
                &chroma_dc,
                &chroma_ac,
            );
        }
    }
    bits.finish();
    output.extend_from_slice(&[0xff, 0xd9]);
    Ok(output)
}

fn jpeg_dimension_error() -> OfficeError {
    OfficeError::unsupported(
        UnsupportedCode::ImageDimensionsExceedLimit,
        "JPEG dimensions exceed the baseline 65535-pixel axis limit",
    )
}

fn write_segment(output: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    output.extend_from_slice(&[0xff, marker]);
    output.extend_from_slice(
        &u16::try_from(payload.len() + 2)
            .expect("bounded JPEG segment")
            .to_be_bytes(),
    );
    output.extend_from_slice(payload);
}

fn write_huffman_segment(output: &mut Vec<u8>, selector: u8, bits: &[u8; 16], values: &[u8]) {
    let mut payload = Vec::with_capacity(17 + values.len());
    payload.push(selector);
    payload.extend_from_slice(bits);
    payload.extend_from_slice(values);
    write_segment(output, 0xc4, &payload);
}

fn pixel(rgb: &[u8], width: u32, height: u32, x: u32, y: u32) -> [u8; 3] {
    let x = x.min(width - 1);
    let y = y.min(height - 1);
    let offset = (u64::from(y) * u64::from(width) + u64::from(x)) as usize * 3;
    [rgb[offset], rgb[offset + 1], rgb[offset + 2]]
}

fn ycbcr([red, green, blue]: [u8; 3]) -> [u8; 3] {
    let red = i32::from(red);
    let green = i32::from(green);
    let blue = i32::from(blue);
    [
        ((19_595 * red + 38_470 * green + 7_471 * blue + 32_768) >> 16) as u8,
        ((-11_059 * red - 21_709 * green + 32_768 * blue + (128 << 16) + 32_767) >> 16) as u8,
        ((32_768 * red - 27_439 * green - 5_329 * blue + (128 << 16) + 32_767) >> 16) as u8,
    ]
}

fn luma_block(rgb: &[u8], width: u32, height: u32, x0: u32, y0: u32) -> [i16; 64] {
    let mut block = [0i16; 64];
    for y in 0..8 {
        for x in 0..8 {
            block[y as usize * 8 + x as usize] =
                i16::from(ycbcr(pixel(rgb, width, height, x0 + x, y0 + y))[0]) - 128;
        }
    }
    block
}

fn chroma_blocks(rgb: &[u8], width: u32, height: u32, x0: u32, y0: u32) -> ([i16; 64], [i16; 64]) {
    let mut cb = [0i16; 64];
    let mut cr = [0i16; 64];
    for y in 0..8 {
        for x in 0..8 {
            let source_x = x0 + x * 2;
            let source_y = y0 + y * 2;
            let samples = [
                ycbcr(pixel(rgb, width, height, source_x, source_y)),
                ycbcr(pixel(rgb, width, height, source_x + 1, source_y)),
                ycbcr(pixel(rgb, width, height, source_x, source_y + 1)),
                ycbcr(pixel(rgb, width, height, source_x + 1, source_y + 1)),
            ];
            // libjpeg's h2v2 box sampler alternates the rounding bias by output column.
            let bias = if ((source_x / 2) & 1) == 0 { 1 } else { 2 };
            let index = y as usize * 8 + x as usize;
            cb[index] = i16::from(
                ((samples
                    .iter()
                    .map(|sample| u16::from(sample[1]))
                    .sum::<u16>()
                    + bias)
                    >> 2) as u8,
            ) - 128;
            cr[index] = i16::from(
                ((samples
                    .iter()
                    .map(|sample| u16::from(sample[2]))
                    .sum::<u16>()
                    + bias)
                    >> 2) as u8,
            ) - 128;
        }
    }
    (cb, cr)
}

fn write_block(
    writer: &mut BitWriter<'_>,
    mut block: [i16; 64],
    quantization: &[u8; 64],
    previous_dc: &mut i16,
    dc_table: &HuffmanTable,
    ac_table: &HuffmanTable,
) {
    jpeg_encoder::fdct(&mut block);
    let mut quantized = [0i16; 64];
    for (destination, &source) in ZIGZAG.iter().enumerate() {
        let value = i32::from(block[source]);
        let divisor = i32::from(quantization[source]) * 8;
        let magnitude = (value.abs() + divisor / 2) / divisor;
        quantized[destination] = if value < 0 { -magnitude } else { magnitude } as i16;
    }

    let difference = quantized[0] - *previous_dc;
    *previous_dc = quantized[0];
    write_coefficient(writer, difference, dc_table, category(difference));

    let mut zero_run = 0u8;
    for &value in &quantized[1..] {
        if value == 0 {
            zero_run += 1;
            continue;
        }
        while zero_run > 15 {
            writer.huffman(ac_table, 0xf0);
            zero_run -= 16;
        }
        let size = category(value);
        writer.huffman(ac_table, (zero_run << 4) | size);
        writer.amplitude(value, size);
        zero_run = 0;
    }
    if zero_run > 0 {
        writer.huffman(ac_table, 0);
    }
}

fn write_coefficient(writer: &mut BitWriter<'_>, value: i16, table: &HuffmanTable, size: u8) {
    writer.huffman(table, size);
    writer.amplitude(value, size);
}

fn category(value: i16) -> u8 {
    if value == 0 {
        0
    } else {
        (16 - value.unsigned_abs().leading_zeros()) as u8
    }
}

struct HuffmanTable {
    codes: [(u16, u8); 256],
}

impl HuffmanTable {
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [(0, 0); 256];
        let mut code = 0u16;
        let mut value_index = 0usize;
        for (length_index, &count) in bits.iter().enumerate() {
            let length = length_index as u8 + 1;
            for _ in 0..count {
                codes[usize::from(values[value_index])] = (code, length);
                code += 1;
                value_index += 1;
            }
            code <<= 1;
        }
        debug_assert_eq!(value_index, values.len());
        Self { codes }
    }
}

struct BitWriter<'a> {
    output: &'a mut Vec<u8>,
    byte: u8,
    used: u8,
}

impl<'a> BitWriter<'a> {
    fn new(output: &'a mut Vec<u8>) -> Self {
        Self {
            output,
            byte: 0,
            used: 0,
        }
    }

    fn huffman(&mut self, table: &HuffmanTable, symbol: u8) {
        let (code, size) = table.codes[usize::from(symbol)];
        debug_assert!(size > 0);
        self.write(u32::from(code), size);
    }

    fn amplitude(&mut self, value: i16, size: u8) {
        if size == 0 {
            return;
        }
        let mask = (1u32 << size) - 1;
        let encoded = if value < 0 {
            (i32::from(value) - 1) as u32 & mask
        } else {
            u32::from(value as u16)
        };
        self.write(encoded, size);
    }

    fn write(&mut self, value: u32, size: u8) {
        for shift in (0..size).rev() {
            self.byte = (self.byte << 1) | ((value >> shift) & 1) as u8;
            self.used += 1;
            if self.used == 8 {
                self.flush_byte();
            }
        }
    }

    fn flush_byte(&mut self) {
        self.output.push(self.byte);
        if self.byte == 0xff {
            self.output.push(0);
        }
        self.byte = 0;
        self.used = 0;
    }

    fn finish(&mut self) {
        if self.used != 0 {
            let padding = 8 - self.used;
            self.byte = (self.byte << padding) | ((1u16 << padding) - 1) as u8;
            self.flush_byte();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encode_word_quality_75_420;

    #[test]
    fn writes_decodable_deterministic_odd_sized_baseline() {
        let rgb = vec![64; 3 * 3 * 5];
        let first = encode_word_quality_75_420(&rgb, 3, 5).expect("encode JPEG");
        let second = encode_word_quality_75_420(&rgb, 3, 5).expect("repeat JPEG");
        let decoded = image::load_from_memory(&first).expect("decode JPEG");
        assert_eq!((decoded.width(), decoded.height()), (3, 5));
        assert_eq!(first, second);
    }
}
