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
    if std::env::args().any(|arg| arg == "--compiler-endpoint-v1") {
        direct_endpoint(directory, phase);
        return;
    }
    let first_worker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("spawned"))
        .is_ok();
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    let mut command = [0; 1];
    let mut request_count = 0;
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
            request_count += 1;
            let stdout = request_count.to_string();
            output.write_all(&0i32.to_le_bytes()).unwrap();
            output
                .write_all(&(stdout.len() as u32).to_le_bytes())
                .unwrap();
            output.write_all(stdout.as_bytes()).unwrap();
            output.write_all(&0u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
        }
        if first_worker && phase == 0 {
            std::fs::write(directory.join("stalled"), b"close").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        if first_worker && phase == 2 && !directory.join("release").exists() {
            std::fs::write(directory.join("stalled"), b"close").unwrap();
            while !directory.join("release").exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
    }
}

// Transport-only frontend controls. These bytes never issue compiler products.
fn direct_endpoint(directory: &std::path::Path, phase: u8) {
    let stall = |stage: &[u8]| {
        std::fs::write(directory.join("stalled"), stage).unwrap();
        loop {
            std::thread::park();
        }
    };
    if phase == 3 {
        stall(b"identity");
    }
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    if phase == 8 {
        output.write_all(b"INVALID!").unwrap();
        output.flush().unwrap();
        return;
    }
    output.write_all(b"TPCID002").unwrap();
    output.write_all(&[1; 32]).unwrap();
    output.write_all(&[2; 32]).unwrap();
    output.flush().unwrap();
    let mut prefix = [0; 8];
    input.read_exact(&mut prefix).unwrap();
    if phase == 13 {
        // A legacy frontend accepts only its old direct transaction grammar.
        // Exit0 on refusal still cannot substitute for the current BEGIN ack.
        if &prefix != b"TPDTR001" {
            return;
        }
    } else {
        assert_eq!(&prefix, b"TPDTR002");
    }
    if phase == 4 {
        stall(b"begin");
    }
    output.write_all(&[1]).unwrap();
    output.flush().unwrap();
    let mut requests = 0u32;
    loop {
        let mut command = [0];
        input.read_exact(&mut command).unwrap();
        if command == [0] {
            if directory.join("input-at-end").exists() {
                let path = std::fs::read_to_string(directory.join("input-at-end")).unwrap();
                let bytes = std::fs::read(path).unwrap();
                std::fs::write(directory.join("observed-input"), bytes).unwrap();
            }
            if phase == 7 || phase == 11 {
                output
                    .write_all(&std::fs::read(directory.join("failure-end")).unwrap())
                    .unwrap();
                output.flush().unwrap();
                if phase == 7 {
                    std::process::exit(17);
                }
            }
            if phase == 6 {
                std::process::exit(17);
            }
            return;
        }
        assert_eq!(command, [1]);
        let _cwd = frame(&mut input);
        let mut argc = [0; 4];
        input.read_exact(&mut argc).unwrap();
        for _ in 0..u32::from_le_bytes(argc) {
            let _ = frame(&mut input);
        }
        requests += 1;
        let body = requests.to_string();
        output.write_all(&0i32.to_le_bytes()).unwrap();
        output
            .write_all(&(body.len() as u32).to_le_bytes())
            .unwrap();
        output.write_all(body.as_bytes()).unwrap();
        output.write_all(&0u32.to_le_bytes()).unwrap();
        output.flush().unwrap();
    }
}
