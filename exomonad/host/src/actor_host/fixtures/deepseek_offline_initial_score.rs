pub fn score(values: &[i32]) -> i32 {
    values.iter().map(|value| value.abs()).sum()
}
