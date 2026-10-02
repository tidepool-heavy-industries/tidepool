use std::io::{Read, Write};

fn frame(input: &mut impl Read) -> Vec<u8> {
    let mut length = [0; 4];
    input.read_exact(&mut length).unwrap();
    let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
    input.read_exact(&mut bytes).unwrap();
    bytes
}

fn main() {
    let executable = std::env::current_exe().unwrap();
    let directory = executable.parent().unwrap();
    let phase = std::fs::read(directory.join("phase")).unwrap()[0];
    let first_worker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("spawned"))
        .is_ok();
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    let mut command = [0; 1];
    loop {
        if input.read_exact(&mut command).is_err() {
            break;
        }
        assert_eq!(command, [1]);
        if first_worker && phase == 1 {
            std::fs::write(directory.join("stalled"), b"begin").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
        loop {
            input.read_exact(&mut command).unwrap();
            if command == [0] {
                break;
            }
            assert_eq!(command, [1]);
            let _cwd = frame(&mut input);
            let mut argc = [0; 4];
            input.read_exact(&mut argc).unwrap();
            for _ in 0..u32::from_le_bytes(argc) {
                let _ = frame(&mut input);
            }
            output.write_all(&[0; 12]).unwrap();
            output.flush().unwrap();
        }
        if first_worker && phase == 0 {
            std::fs::write(directory.join("stalled"), b"close").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
    }
}
