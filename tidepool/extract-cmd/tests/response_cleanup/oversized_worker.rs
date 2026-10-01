use std::io::{Read, Write};

fn frame(input: &mut impl Read) {
    let mut size = [0; 4];
    input.read_exact(&mut size).unwrap();
    let mut bytes = vec![0; u32::from_le_bytes(size) as usize];
    input.read_exact(&mut bytes).unwrap();
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--print-worker-request-flag") {
        print!("{}", std::env::var("TIDEPOOL_TEST_WORKER_FLAG").unwrap());
        return;
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut command = [0];
    input.read_exact(&mut command).unwrap();
    assert_eq!(command, [1]);
    output.write_all(&[1]).unwrap();
    output.flush().unwrap();
    input.read_exact(&mut command).unwrap();
    assert_eq!(command, [1]);
    frame(&mut input);
    let mut count = [0; 4];
    input.read_exact(&mut count).unwrap();
    for _ in 0..u32::from_le_bytes(count) {
        frame(&mut input);
    }
    std::fs::write(
        std::env::var_os("TIDEPOOL_TEST_WORKER_PID").unwrap(),
        std::process::id().to_string(),
    )
    .unwrap();
    let mut requests = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var_os("TIDEPOOL_TEST_REQUEST_COUNT").unwrap())
        .unwrap();
    requests.write_all(b"1\n").unwrap();
    output.write_all(&0i32.to_le_bytes()).unwrap();
    output.write_all(&u32::MAX.to_le_bytes()).unwrap();
    output.flush().unwrap();
    // More than the pipe capacity: graceful stdin close cannot settle this
    // writer while the frontend has stopped draining the rejected frame.
    let body = [b'x'; 128 * 1024];
    while output.write_all(&body).is_ok() {}
}
