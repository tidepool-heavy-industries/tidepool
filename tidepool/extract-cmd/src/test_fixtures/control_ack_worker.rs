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
    if executable.file_name().unwrap() == "launcher" {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::process::CommandExt;
        let frontend =
            std::ffi::OsString::from_vec(std::fs::read(directory.join("frontend-path")).unwrap());
        let worker = directory.join("worker");
        let error = std::process::Command::new(frontend)
            .env("TIDEPOOL_EXTRACT_WORKER", worker)
            .args(std::env::args_os().skip(1))
            .exec();
        panic!("actual frontend exec failed: {}", error);
    }
    let phase = std::fs::read(directory.join("phase")).unwrap()[0];
    if std::env::args().any(|arg| arg == "--print-worker-request-flag") {
        print!(
            "{}",
            std::fs::read_to_string(directory.join("request-flag")).unwrap()
        );
        return;
    }
    if std::env::args().any(|arg| arg == "--compiler-endpoint-v1") {
        direct_endpoint(directory, phase);
        return;
    }
    let first_worker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("spawned"))
        .is_ok();
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("worker-pids"))
            .unwrap(),
        "{}",
        std::process::id()
    )
    .unwrap();
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
            let native = command == [6];
            assert!(native || command == [1]);
            let mut context = [0; 144];
            if native {
                input.read_exact(&mut context).unwrap();
                assert_eq!(&context[..8], b"TPNDX001");
            }
            let _cwd = frame(&mut input);
            let mut argc = [0; 4];
            input.read_exact(&mut argc).unwrap();
            for _ in 0..u32::from_le_bytes(argc) {
                let _ = frame(&mut input);
            }
            if native {
                native_exchange(directory, phase, &context, &mut input, &mut output);
                continue;
            }
            if first_worker && phase == 18 {
                std::fs::write(directory.join("stalled"), b"request").unwrap();
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
            request_count += 1;
            let stdout = if first_worker && phase == 17 {
                "body-report-v1".to_owned()
            } else {
                request_count.to_string()
            };
            let stderr: &[u8] = if first_worker && phase == 17 {
                b"visible diagnostic v1\ntidepool-timing phase=fixture ms=1\n"
            } else {
                b""
            };
            let code: i32 = if first_worker && phase == 17 { 23 } else { 0 };
            output.write_all(&code.to_le_bytes()).unwrap();
            output
                .write_all(&(stdout.len() as u32).to_le_bytes())
                .unwrap();
            output.write_all(stdout.as_bytes()).unwrap();
            output
                .write_all(&(stderr.len() as u32).to_le_bytes())
                .unwrap();
            output.write_all(stderr).unwrap();
            output.flush().unwrap();
        }
        if first_worker && phase == 0 {
            std::fs::write(directory.join("stalled"), b"close").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        if first_worker && (phase == 2 || phase == 17) && !directory.join("release").exists() {
            std::fs::write(directory.join("stalled"), b"close").unwrap();
            while !directory.join("release").exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if first_worker && phase == 17 {
            std::process::exit(17);
        }
        output.write_all(&[1]).unwrap();
        output.flush().unwrap();
    }
    if phase == 19 {
        std::process::exit(17);
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
        assert_eq!(&prefix, b"TPDTR003");
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
            if phase == 16 {
                stall(b"end");
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
        let native = command == [6];
        assert!(native || command == [1]);
        let mut context = [0; 144];
        if native {
            input.read_exact(&mut context).unwrap();
        }
        let _cwd = frame(&mut input);
        let mut argc = [0; 4];
        input.read_exact(&mut argc).unwrap();
        for _ in 0..u32::from_le_bytes(argc) {
            let _ = frame(&mut input);
        }
        if native {
            output.write_all(&[4]).unwrap();
            output.write_all(&context).unwrap();
            output.write_all(&0u64.to_le_bytes()).unwrap();
            output.write_all(&1u64.to_le_bytes()).unwrap();
            output.write_all(&0u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
            stall(b"reply-not-read");
        }
        if phase == 14 {
            return;
        }
        requests += 1;
        let body = requests.to_string();
        let exit_code: i32 = if phase == 15 { 1 } else { 0 };
        output.write_all(&exit_code.to_le_bytes()).unwrap();
        output
            .write_all(&(body.len() as u32).to_le_bytes())
            .unwrap();
        output.write_all(body.as_bytes()).unwrap();
        output.write_all(&0u32.to_le_bytes()).unwrap();
        output.flush().unwrap();
    }
}

// Transport fixture: no compiler products or semantic authority are issued.
fn native_exchange(
    directory: &std::path::Path,
    phase: u8,
    context: &[u8; 144],
    input: &mut impl Read,
    output: &mut impl Write,
) {
    let rounds = if phase == 20 { 2 } else { 1 };
    for sequence in 1..=rounds {
        let stage = if sequence == 1 { 0u64 } else { 7u64 };
        let sequence = sequence as u64;
        output.write_all(&[4]).unwrap();
        let mut observed = *context;
        if phase == 22 {
            observed[40] ^= 1;
        }
        output.write_all(&observed).unwrap();
        output.write_all(&stage.to_le_bytes()).unwrap();
        output.write_all(&sequence.to_le_bytes()).unwrap();
        if phase == 21 {
            output.write_all(&100u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
            std::process::exit(0);
        }
        let payload = format!("selected-stage-{stage}");
        output
            .write_all(&(payload.len() as u32).to_le_bytes())
            .unwrap();
        output.write_all(payload.as_bytes()).unwrap();
        if phase == 25 {
            output.write_all(&[0]).unwrap();
        }
        output.flush().unwrap();
        std::fs::write(directory.join("demand-written"), sequence.to_le_bytes()).unwrap();
        let mut reply = [0; 162];
        input.read_exact(&mut reply).unwrap();
        assert_eq!(reply[0], 5);
        assert_eq!(&reply[1..145], context);
        assert_eq!(&reply[145..153], &stage.to_le_bytes());
        assert_eq!(&reply[153..161], &sequence.to_le_bytes());
        let body = frame(input);
        std::fs::write(directory.join(format!("reply-{sequence}")), &body).unwrap();
        if reply[161] == 1 {
            output.write_all(&[0]).unwrap();
            output.write_all(&1i32.to_le_bytes()).unwrap();
            output.write_all(&0u32.to_le_bytes()).unwrap();
            output.write_all(&0u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
            return;
        }
        assert_eq!(body, format!("resolved-{stage}").as_bytes());
        if phase == 23 {
            output.write_all(&[4]).unwrap();
            output.write_all(context).unwrap();
            output.write_all(&stage.to_le_bytes()).unwrap();
            output.write_all(&sequence.to_le_bytes()).unwrap();
            output.write_all(&0u32.to_le_bytes()).unwrap();
            output.flush().unwrap();
            let mut never = [0];
            input.read_exact(&mut never).unwrap();
        }
    }
    output
        .write_all(&[if phase == 24 { 2 } else { 0 }])
        .unwrap();
    output.write_all(&0i32.to_le_bytes()).unwrap();
    let result = b"native-complete";
    output
        .write_all(&(result.len() as u32).to_le_bytes())
        .unwrap();
    output.write_all(result).unwrap();
    output.write_all(&0u32.to_le_bytes()).unwrap();
    output.flush().unwrap();
}
