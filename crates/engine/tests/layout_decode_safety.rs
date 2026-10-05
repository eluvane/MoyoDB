use moyodb_engine::layout::unsafe_read_struct;
use moyodb_engine::EngineError;

#[test]
fn plain_data_decode_accepts_unaligned_prefix_and_ignores_suffix() {
    let bytes = [0xff, 0x12, 0x34, 0x56, 0x78, 0xee];
    let value = unsafe_read_struct::<u32>(&bytes[1..]).unwrap();
    assert_eq!(value, u32::from_ne_bytes([0x12, 0x34, 0x56, 0x78]));
}

#[test]
fn plain_data_decode_reports_short_inputs_deterministically() {
    let error = unsafe_read_struct::<u64>(&[0; 7]).unwrap_err();
    assert_eq!(
        error,
        EngineError::Serialization("short struct read: need 8, got 7".into())
    );
}
