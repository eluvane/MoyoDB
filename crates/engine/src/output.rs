use crate::error::{EngineError, Result};
#[cfg(any(target_arch = "wasm32", test))]
use crate::value::ValueState;

pub(crate) const MISSING_VALUE: u32 = u32::MAX;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn pack_value_states(states: &[Option<ValueState>]) -> Result<Vec<u8>> {
    let count = u32::try_from(states.len())
        .map_err(|_| EngineError::Serialization("value states output has too many rows".into()))?;
    let length = states
        .len()
        .checked_mul(32)
        .and_then(|length| length.checked_add(4))
        .ok_or_else(|| EngineError::Serialization("value states output length overflow".into()))?;
    let mut packet = Vec::new();
    packet.try_reserve_exact(length).map_err(|error| {
        EngineError::Storage(format!("value states output allocation failed: {error}"))
    })?;
    packet.resize(length, 0);
    packet[..4].copy_from_slice(&count.to_le_bytes());
    for (index, state) in states.iter().enumerate() {
        let Some(state) = state else {
            continue;
        };
        let offset = 4 + index * 32;
        let flags = u32::from(state.exists)
            | (u32::from(state.expired) << 1)
            | (u32::from(state.revision.is_some()) << 2);
        let max_length = u32::try_from(state.max_length)
            .map_err(|_| EngineError::ValueTooLarge(state.max_length))?;
        packet[offset..offset + 4].copy_from_slice(&flags.to_le_bytes());
        packet[offset + 4..offset + 12]
            .copy_from_slice(&state.expires_at_ms.unwrap_or(0).to_le_bytes());
        if let Some(revision) = state.revision.as_ref() {
            packet[offset + 12..offset + 20].copy_from_slice(&revision.epoch.to_le_bytes());
            packet[offset + 20..offset + 28].copy_from_slice(&revision.ordinal.to_le_bytes());
        }
        packet[offset + 28..offset + 32].copy_from_slice(&max_length.to_le_bytes());
    }
    Ok(packet)
}

pub(crate) struct PackedValues {
    pub bytes: Vec<u8>,
    offsets: Vec<usize>,
}

impl PackedValues {
    pub fn new(lengths: &[Option<usize>]) -> Result<Self> {
        let count = u32::try_from(lengths.len())
            .map_err(|_| EngineError::Serialization("packed output has too many values".into()))?;
        let header_len = lengths
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(|| EngineError::Serialization("packed output length overflow".into()))?;
        let mut total = header_len;
        let mut offsets = Vec::with_capacity(lengths.len());
        for length in lengths {
            offsets.push(total);
            if let Some(length) = length {
                if *length >= MISSING_VALUE as usize {
                    return Err(EngineError::ValueTooLarge(*length));
                }
                total = total.checked_add(*length).ok_or_else(|| {
                    EngineError::Serialization("packed output length overflow".into())
                })?;
            }
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(total).map_err(|error| {
            EngineError::Storage(format!("packed output allocation failed: {error}"))
        })?;
        bytes.resize(total, 0);
        bytes[..4].copy_from_slice(&count.to_le_bytes());
        for (index, length) in lengths.iter().enumerate() {
            let length = length.map_or(MISSING_VALUE, |length| length as u32);
            bytes[4 + index * 4..8 + index * 4].copy_from_slice(&length.to_le_bytes());
        }
        Ok(Self { bytes, offsets })
    }

    pub fn writer(&mut self, index: usize, length: usize) -> SliceWriter<'_> {
        let offset = self.offsets[index];
        SliceWriter {
            bytes: &mut self.bytes[offset..offset + length],
            written: 0,
        }
    }

    pub fn duplicate(&mut self, source: usize, target: usize, length: usize) {
        let offset = self.offsets[source];
        self.bytes
            .copy_within(offset..offset + length, self.offsets[target]);
    }
}

pub(crate) struct SliceWriter<'a> {
    bytes: &'a mut [u8],
    written: usize,
}

impl SliceWriter<'_> {
    pub fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let end = self
            .written
            .checked_add(chunk.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| EngineError::Corruption("value exceeds packed output slot".into()))?;
        self.bytes[self.written..end].copy_from_slice(chunk);
        self.written = end;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        if self.written != self.bytes.len() {
            return Err(EngineError::Corruption(
                "value does not fill packed output slot".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct PackedScanPage {
    pub packet: Vec<u8>,
    pub row_count: usize,
    pub last_key: Option<Vec<u8>>,
    pub exhausted: bool,
}

pub(crate) struct ScanPacket {
    pub bytes: Vec<u8>,
    pub rows: usize,
    pub last_key: Option<Vec<u8>>,
    max_bytes: usize,
    budget_bytes: usize,
}

impl ScanPacket {
    pub fn new(max_bytes: usize) -> Result<Self> {
        if max_bytes < 4 {
            return Err(EngineError::InvalidRange(
                "scan byte budget must include the 4-byte header".into(),
            ));
        }
        Ok(Self {
            bytes: vec![0; 4],
            rows: 0,
            last_key: None,
            max_bytes,
            budget_bytes: 4,
        })
    }

    pub fn begin_row(&mut self, key: &[u8], value_len: usize, decoded_len: usize) -> Result<bool> {
        let key_len = u32::try_from(key.len()).map_err(|_| EngineError::KeyTooLarge(key.len()))?;
        let encoded_value_len =
            u32::try_from(value_len).map_err(|_| EngineError::ValueTooLarge(value_len))?;
        let end = self
            .budget_bytes
            .checked_add(8)
            .and_then(|n| n.checked_add(key.len()))
            .and_then(|n| n.checked_add(value_len.max(decoded_len)))
            .ok_or_else(|| EngineError::Serialization("scan output length overflow".into()))?;
        if end > self.max_bytes {
            if self.rows == 0 {
                return Err(EngineError::ValueTooLarge(end));
            }
            return Ok(false);
        }
        let required = self.bytes.len() + 8 + key.len() + value_len;
        if required > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(required)
                .min(self.max_bytes);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|error| {
                    EngineError::Storage(format!("scan output allocation failed: {error}"))
                })?;
        }
        self.budget_bytes = end;
        self.bytes.extend_from_slice(&key_len.to_le_bytes());
        self.bytes
            .extend_from_slice(&encoded_value_len.to_le_bytes());
        self.bytes.extend_from_slice(key);
        Ok(true)
    }

    pub fn finish_row(&mut self, key: &[u8]) {
        self.rows += 1;
        self.last_key = Some(key.to_vec());
    }

    pub fn finish(mut self, exhausted: bool) -> Result<PackedScanPage> {
        let count = u32::try_from(self.rows)
            .map_err(|_| EngineError::Serialization("scan output has too many rows".into()))?;
        self.bytes[..4].copy_from_slice(&count.to_le_bytes());
        Ok(PackedScanPage {
            packet: self.bytes,
            row_count: self.rows,
            last_key: self.last_key,
            exhausted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ValueRevision;

    #[test]
    fn state_packet_preserves_flags_and_full_width_revision_fields() -> Result<()> {
        let states = [
            None,
            Some(ValueState {
                exists: true,
                expired: false,
                expires_at_ms: None,
                revision: None,
                max_length: 7,
            }),
            Some(ValueState {
                exists: false,
                expired: true,
                expires_at_ms: Some(u64::MAX - 1),
                revision: Some(ValueRevision {
                    epoch: u64::MAX - 2,
                    ordinal: u64::MAX - 3,
                }),
                max_length: 0,
            }),
        ];
        let packet = pack_value_states(&states)?;
        assert_eq!(packet.len(), 4 + 3 * 32);
        assert_eq!(&packet[..4], &3u32.to_le_bytes());
        assert_eq!(&packet[4..36], &[0; 32]);
        assert_eq!(&packet[36..40], &1u32.to_le_bytes());
        assert_eq!(&packet[64..68], &7u32.to_le_bytes());
        assert_eq!(&packet[68..72], &6u32.to_le_bytes());
        assert_eq!(&packet[72..80], &(u64::MAX - 1).to_le_bytes());
        assert_eq!(&packet[80..88], &(u64::MAX - 2).to_le_bytes());
        assert_eq!(&packet[88..96], &(u64::MAX - 3).to_le_bytes());
        assert_eq!(&packet[96..100], &0u32.to_le_bytes());
        Ok(())
    }

    #[test]
    fn optional_packet_preserves_missing_empty_order_and_duplicates() -> Result<()> {
        let mut packet = PackedValues::new(&[Some(3), None, Some(0), Some(3)])?;
        let mut writer = packet.writer(0, 3);
        writer.write(&[7])?;
        writer.write(&[8, 9])?;
        writer.finish()?;
        packet.duplicate(0, 3, 3);
        let mut expected = vec![4, 0, 0, 0, 3, 0, 0, 0];
        expected.extend_from_slice(&MISSING_VALUE.to_le_bytes());
        expected.extend_from_slice(&[0, 0, 0, 0, 3, 0, 0, 0, 7, 8, 9, 7, 8, 9]);
        assert_eq!(packet.bytes, expected);
        Ok(())
    }

    #[test]
    fn scan_packet_enforces_wire_bytes_before_accepting_a_row() -> Result<()> {
        let mut packet = ScanPacket::new(15)?;
        assert!(packet.begin_row(b"k", 2, 2)?);
        packet.bytes.extend_from_slice(b"vv");
        packet.finish_row(b"k");
        assert!(!packet.begin_row(b"x", 0, 0)?);
        let page = packet.finish(false)?;
        assert_eq!(
            page.packet,
            [1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, b'k', b'v', b'v']
        );
        assert_eq!(
            ScanPacket::new(14)?.begin_row(b"k", 2, 2),
            Err(EngineError::ValueTooLarge(15))
        );
        Ok(())
    }
}
