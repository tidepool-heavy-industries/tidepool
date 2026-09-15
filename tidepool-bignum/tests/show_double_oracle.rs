use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires the repository's pinned GHC toolchain"]
fn formatting_matches_native_ghc_at_rounding_boundaries() {
    let mut bits = vec![0, 1, 2, (1_u64 << 52) - 1, 1_u64 << 52, f64::MAX.to_bits()];
    for exponent in -323..=308 {
        let value = format!("1e{exponent}").parse::<f64>().unwrap().to_bits();
        bits.extend([value - 1, value, value + 1]);
    }
    for exponent in [1_u64, 2, 3, 512, 1022, 1023, 1024, 1536, 2045, 2046] {
        let value = exponent << 52;
        bits.extend([value - 1, value, value + 1]);
    }
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    for _ in 0..512 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bits.push(state);
    }
    bits.extend(bits.clone().into_iter().map(|value| value | (1_u64 << 63)));
    let input = bits
        .iter()
        .map(|bits| format!("{bits}\n"))
        .collect::<String>();
    let mut child = Command::new("runghc")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/show_double_oracle.hs"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run in the repository dev shell with runghc available");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let expected: Vec<_> = output.lines().collect();
    assert_eq!(expected.len(), bits.len());
    for (bits, expected) in bits.into_iter().zip(expected) {
        assert_eq!(
            tidepool_bignum::haskell_show_double(f64::from_bits(bits)),
            expected,
            "bits {bits:016x}"
        );
    }
}
