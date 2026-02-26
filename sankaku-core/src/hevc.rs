/// Start code used by the iOS capture stream (`00 00 00 01`).
const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];
const HEVC_NAL_HEADER_LEN: usize = 2;
const HEURISTIC_SCAN_LIMIT_BYTES: usize = 512;

/// HEVC SAO parameters extracted from slice-level syntax.
///
/// `#[repr(C)]` keeps this layout C-FFI friendly for OpenZL interop.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaoParameters {
    pub ctu_x: u16,
    pub ctu_y: u16,
    /// 0 = not applied, 1 = band offset, 2 = edge offset
    pub sao_type_idx: u8,
    pub band_position: u8,
    pub offset: [i8; 4],
}

/// Zero-copy iterator over Annex B NAL units.
///
/// Each yielded item excludes the `00 00 00 01` start code and borrows from
/// the original byte slice.
pub struct AnnexBNalIter<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> AnnexBNalIter<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }
}

impl<'a> Iterator for AnnexBNalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        while self.cursor < self.data.len() {
            let start = find_start_code(self.data, self.cursor)?;
            let nal_start = start + ANNEX_B_START_CODE.len();

            let next_start = find_start_code(self.data, nal_start);
            let nal_end = next_start.unwrap_or(self.data.len());
            self.cursor = next_start.unwrap_or(self.data.len());

            if nal_start < nal_end {
                return Some(&self.data[nal_start..nal_end]);
            }
        }
        None
    }
}

/// Creates a zero-copy iterator over Annex B NAL units.
pub fn split_annex_b(data: &[u8]) -> AnnexBNalIter<'_> {
    AnnexBNalIter::new(data)
}

/// Backward-compatible alias for `split_annex_b`.
pub fn annex_b_nal_units(data: &[u8]) -> AnnexBNalIter<'_> {
    split_annex_b(data)
}

/// Extracts HEVC NAL unit type from the first NAL header byte.
///
/// Formula: `(nal_header_byte_0 >> 1) & 0x3F`
pub fn nal_unit_type(nal_unit: &[u8]) -> Option<u8> {
    let header_byte = *nal_unit.first()?;
    Some((header_byte >> 1) & 0x3F)
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new_at(data: &'a [u8], bit_pos: usize) -> Self {
        Self { data, bit_pos }
    }

    fn bits_remaining(&self) -> usize {
        self.data
            .len()
            .saturating_mul(8)
            .saturating_sub(self.bit_pos)
    }

    fn read_bit(&mut self) -> Option<u8> {
        if self.bit_pos >= self.data.len().saturating_mul(8) {
            return None;
        }

        let byte = self.data[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos = self.bit_pos.saturating_add(1);
        Some((byte >> shift) & 0x01)
    }

    fn read_bits(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.bits_remaining() < n {
            return None;
        }

        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0usize;
        while self.read_bit()? == 0 {
            leading_zero_bits = leading_zero_bits.saturating_add(1);
            if leading_zero_bits > 31 {
                return None;
            }
        }

        if leading_zero_bits == 0 {
            return Some(0);
        }

        let suffix = self.read_bits(leading_zero_bits)?;
        Some(((1u32 << leading_zero_bits) - 1) + suffix)
    }
}

/// Converts NAL payload EBSP into RBSP by removing emulation prevention bytes.
fn ebsp_to_rbsp(ebsp: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(ebsp.len());
    let mut zero_run = 0usize;

    for &byte in ebsp {
        if zero_run >= 2 && byte == 0x03 {
            zero_run = 0;
            continue;
        }

        rbsp.push(byte);
        if byte == 0 {
            zero_run = zero_run.saturating_add(1);
        } else {
            zero_run = 0;
        }
    }

    rbsp
}

fn try_parse_sao_candidate(
    rbsp: &[u8],
    start_byte: usize,
    prelude_ue_count: usize,
    expect_sao_flag: bool,
) -> Option<SaoParameters> {
    let mut reader = BitReader::new_at(rbsp, start_byte.saturating_mul(8));

    // Heuristic placeholder for slice segment address-like value.
    let mut ctu_address_hint = 0u32;
    for _ in 0..prelude_ue_count {
        ctu_address_hint = reader.read_ue()?;
        if ctu_address_hint > (1 << 20) {
            return None;
        }
    }

    if expect_sao_flag && reader.read_bit()? == 0 {
        return None;
    }

    let sao_type_idx = u8::try_from(reader.read_ue()?).ok()?;
    if !(1..=2).contains(&sao_type_idx) {
        return None;
    }

    let mut offsets = [0i8; 4];
    let mut non_zero_offsets = 0usize;
    for offset in &mut offsets {
        let magnitude = reader.read_ue()?;
        if magnitude > 7 {
            return None;
        }

        if magnitude == 0 {
            *offset = 0;
            continue;
        }

        let sign = reader.read_bit()? == 1;
        let value = i8::try_from(magnitude).ok()?;
        *offset = if sign { -value } else { value };
        non_zero_offsets = non_zero_offsets.saturating_add(1);
    }

    if non_zero_offsets == 0 {
        return None;
    }

    let band_position = if sao_type_idx == 1 {
        u8::try_from(reader.read_bits(5)?).ok()?
    } else {
        u8::try_from(reader.read_bits(2)?).ok()?
    };

    Some(SaoParameters {
        ctu_x: (ctu_address_hint & 0xFFFF) as u16,
        ctu_y: ((ctu_address_hint >> 16) & 0xFFFF) as u16,
        sao_type_idx,
        band_position,
        offset: offsets,
    })
}

fn heuristic_scan_sao(rbsp: &[u8]) -> Option<SaoParameters> {
    let scan_limit = rbsp.len().min(HEURISTIC_SCAN_LIMIT_BYTES);
    for start_byte in 0..scan_limit {
        for prelude_ue_count in 0..=2 {
            for expect_sao_flag in [true, false] {
                if let Some(params) =
                    try_parse_sao_candidate(rbsp, start_byte, prelude_ue_count, expect_sao_flag)
                {
                    return Some(params);
                }
            }
        }
    }
    None
}

/// SAO extraction scanner for VCL NAL units.
///
/// Returns `None` for non-VCL units (types 32..63).
pub fn extract_sao_parameters(nal_unit: &[u8]) -> Option<SaoParameters> {
    let nal_type = nal_unit_type(nal_unit)?;
    if nal_type > 31 {
        return None;
    }

    let payload = nal_unit.get(HEVC_NAL_HEADER_LEN..)?;
    if payload.is_empty() {
        return None;
    }

    let rbsp = ebsp_to_rbsp(payload);

    // TODO(hevc/sao/full-decode):
    // Replace this heuristic scanner with stateful SPS/PPS-aware slice header
    // parsing once parameter-set tracking is integrated into the stream parser.
    heuristic_scan_sao(&rbsp)
}

fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    if from >= data.len() {
        return None;
    }
    data[from..]
        .windows(ANNEX_B_START_CODE.len())
        .position(|window| window == ANNEX_B_START_CODE)
        .map(|relative| from + relative)
}

#[cfg(test)]
mod tests {
    use super::{BitReader, ebsp_to_rbsp, extract_sao_parameters, nal_unit_type, split_annex_b};

    #[test]
    fn annex_b_splitter_and_nal_type_scanner_work() {
        let data = [
            0x00, 0x00, 0x00, 0x01, // start
            0x40, 0x01, 0xAA, 0xBB, // nal type 32 (VPS)
            0x00, 0x00, 0x00, 0x01, // start
            0x02, 0x01, 0xCC, // nal type 1 (VCL)
        ];

        let units: Vec<&[u8]> = split_annex_b(&data).collect();
        assert_eq!(units.len(), 2);

        let first_type = nal_unit_type(units[0]).expect("first unit should have header");
        let second_type = nal_unit_type(units[1]).expect("second unit should have header");
        assert_eq!(first_type, 32);
        assert_eq!(second_type, 1);

        assert!(
            extract_sao_parameters(units[0]).is_none(),
            "non-VCL unit should not return SAO"
        );
    }

    #[test]
    fn emulation_prevention_unescape_works() {
        let ebsp = [0x00, 0x00, 0x03, 0x01, 0x11, 0x00, 0x00, 0x03, 0x02];
        let rbsp = ebsp_to_rbsp(&ebsp);
        assert_eq!(rbsp, vec![0x00, 0x00, 0x01, 0x11, 0x00, 0x00, 0x02]);
    }

    #[test]
    fn bit_reader_reads_unsigned_exp_golomb() {
        // ue(v): 0 => "1", 1 => "010", 2 => "011" => bits: 1010011[0]
        let data = [0b1010_0110];
        let mut reader = BitReader::new_at(&data, 0);
        assert_eq!(reader.read_ue(), Some(0));
        assert_eq!(reader.read_ue(), Some(1));
        assert_eq!(reader.read_ue(), Some(2));
    }

    #[test]
    fn heuristic_sao_extraction_finds_vcl_candidate() {
        // VCL NAL header (type 1): 0x02, 0x01
        // Payload bits (byte-aligned start):
        // sao_type_idx=1 (ue=010)
        // offsets abs/sign: +1, -1, 0, 0
        // band_position=5 (00101)
        let vcl_nal = [0x02, 0x01, 0x48, 0xB9, 0x40];
        let sao = extract_sao_parameters(&vcl_nal).expect("expected SAO candidate");
        assert_eq!(sao.sao_type_idx, 1);
        assert_eq!(sao.band_position, 5);
        assert_eq!(sao.offset, [1, -1, 0, 0]);
    }
}
