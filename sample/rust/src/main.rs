use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{prelude::*, Result, SeekFrom},
    mem::size_of,
    net::{TcpListener, TcpStream},
};

fn main() -> Result<()> {
    let mut file = File::open("/var/log/dpkg.log")?;
    println!(">>>>> open file {file:#?}");

    let mut buffer = vec![0; u16::MAX as usize];
    let read_count = file.read(&mut buffer)?;
    println!(">>>>> read {read_count:#?} bytes from file {file:#?}");

    let new_start = file.seek(SeekFrom::Start(10))?;
    println!(">>> seek now starts at {new_start:#?} for file {file:#?}");

    let mut write_file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open("/tmp/rust_sample.txt")
        .unwrap();
    println!(">>>>> created file with write permission {write_file:#?}");

    let written_count = write_file.write("mirrord sample rust".as_bytes()).unwrap();
    println!(">>>>> written {written_count:#?} bytes to file {write_file:#?}");

    write_file.seek(SeekFrom::Start(0)).unwrap();
    println!(">>>>> seeking file back to start {write_file:#?}");

    let mut read_buf = vec![0; 128];
    let read_count = write_file.read(&mut read_buf).unwrap();
    println!(">>>>> read {read_count:#?} bytes from file {write_file:#?}");

    let read_str = String::from_utf8_lossy(&read_buf);
    println!(">>>>> read {read_str:#?} from file {write_file:#?}");

    unsafe {
        let filepath = CString::new("/tmp/rust_sample.txt").unwrap();
        let file_mode = CString::new("r").unwrap();
        let file_ptr = libc::fopen(filepath.as_ptr(), file_mode.as_ptr());

        let file_fd: i32 = *(file_ptr as *const _);
        println!(">>>>> fopen local fd {file_fd:#?}");

        let mut buffer = vec![0; 128];
        let read_amount = libc::fread(buffer.as_mut_ptr().cast(), size_of::<u8>(), 128, file_ptr);
        println!(">>>>> fread read {read_amount:#?} bytes from file {filepath:#?}");

        let read_str = String::from_utf8_lossy(&buffer);
        println!(">>>>> read {read_str:#?} from file {filepath:#?}");
    }

    let dir = File::open("/var/log/")?;
    println!(
        ">>>>> open directory {:#?} with {:#?} and is directory ? {:#?} ",
        dir,
        dir.metadata(),
        dir.metadata().unwrap().is_dir()
    );

    let listener = TcpListener::bind("127.0.0.1:80")?;
    for stream in listener.incoming() {
        let stream = stream.unwrap();

        handle_connection(stream);
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    let _ = stream.read(&mut buffer).unwrap();

    println!(">>>>> request is {}", String::from_utf8_lossy(&buffer[..]));
}
