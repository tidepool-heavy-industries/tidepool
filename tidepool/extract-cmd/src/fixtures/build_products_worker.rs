use std::io::{Read, Write};
use std::path::PathBuf;

fn u32(r: &mut impl Read) -> usize {
    let mut b = [0; 4];
    r.read_exact(&mut b).unwrap();
    u32::from_le_bytes(b) as usize
}
fn frame(r: &mut impl Read) -> Vec<u8> {
    let mut b = vec![0; u32(r)];
    r.read_exact(&mut b).unwrap();
    b
}
fn request(payload: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(payload).unwrap();
    let bytes: Vec<_> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect();
    let mut r = &bytes[17..];
    let mut input = None;
    let mut root = None;
    for _ in 0..u32(&mut r) {
        let mut tag = [0];
        r.read_exact(&mut tag).unwrap();
        let value = PathBuf::from(String::from_utf8(frame(&mut r)).unwrap());
        match tag[0] {
            1 => input = Some(value),
            25 => root = Some(value),
            other => panic!("unexpected fixture field {other}"),
        }
    }
    let root = root.unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let product = root.join("Expr.hi");
    let previous = std::fs::read_to_string(&product).unwrap_or_default();
    let content = std::fs::read_to_string(input.unwrap()).unwrap();
    std::fs::write(&product, &content).unwrap();
    // Concurrent writers under a shared directory can overwrite this request.
    std::thread::sleep(std::time::Duration::from_millis(80));
    let actual = std::fs::read_to_string(&product).unwrap();
    format!(
        "{}\n{previous}\n{actual}\n{}",
        root.display(),
        std::process::id()
    )
    .into_bytes()
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--observe-owned-environment") {
        let names = [
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
            "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
            "TIDEPOOL_PERFORMANCE_COMPILER_TRACE",
        ];
        for name in names {
            println!(
                "{name}={}",
                std::env::var(name).expect("owner-issued input")
            );
        }
        std::fs::copy(&args[2], &args[3]).unwrap();
        std::process::exit(args[4].parse().unwrap());
    }
    if args.get(1).map(String::as_str) == Some("--print-worker-request-flag") {
        println!("--worker-request-v18");
        return;
    }
    if args.get(1).map(String::as_str) == Some("--worker-request-v18") {
        std::io::stdout()
            .write_all(&request(args[2].as_bytes()))
            .unwrap();
        return;
    }
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut one = [0];
    while stdin.read_exact(&mut one).is_ok() {
        assert_eq!(one, [1]);
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
        loop {
            stdin.read_exact(&mut one).unwrap();
            if one == [0] {
                break;
            }
            assert_eq!(one, [1]);
            let cwd = String::from_utf8(frame(&mut stdin)).unwrap();
            std::env::set_current_dir(cwd).unwrap();
            assert_eq!(u32(&mut stdin), 2);
            let _flag = frame(&mut stdin);
            let out = request(&frame(&mut stdin));
            stdout.write_all(&0i32.to_le_bytes()).unwrap();
            stdout.write_all(&(out.len() as u32).to_le_bytes()).unwrap();
            stdout.write_all(&out).unwrap();
            stdout.write_all(&0u32.to_le_bytes()).unwrap();
            stdout.flush().unwrap();
        }
        stdout.write_all(&[1]).unwrap();
        stdout.flush().unwrap();
    }
}
