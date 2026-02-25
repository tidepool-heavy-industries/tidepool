#[test]
fn test_utf8_len_logic() {
    fn clz8(v: u8) -> i64 {
        let masked = v as u64;
        let clz64 = masked.leading_zeros() as i64;
        clz64 - 56
    }
    fn y_logic(r: u8) -> i64 {
        let c = clz8(!r);
        c ^ if c <= 0 { 1 } else { 0 }
    }

    assert_eq!(y_logic(0x61), 1); // 'a'
    assert_eq!(y_logic(0xC3), 2); // start of 2-byte
    assert_eq!(y_logic(0xE2), 3); // start of 3-byte
    assert_eq!(y_logic(0xF0), 4); // start of 4-byte
    assert_eq!(y_logic(0x20), 1); // space
}
