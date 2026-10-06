//! Headless check: connect, wait for the first full frame, save it as a PPM.
//! Usage: cargo run --example probe -- host:port [password] [out.ppm]

#[allow(dead_code)]
#[path = "../src/rfb.rs"]
mod rfb;

use std::sync::mpsc::channel;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).map(String::as_str).unwrap_or("localhost:5900");
    let password = args.get(2).map(String::as_str).unwrap_or("");
    let out = args.get(3).map(String::as_str).unwrap_or("frame.ppm");

    let conn = rfb::Connection::connect(addr, password)?;
    let fb = conn.fb.clone();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        conn.run(move || {
            let _ = tx.send(());
        })
    });
    rx.recv()?;

    let fb = fb.lock().unwrap();
    println!("desktop {:?}: {}x{}", fb.name, fb.width, fb.height);
    let mut ppm = format!("P6 {} {} 255\n", fb.width, fb.height).into_bytes();
    for px in fb.pixels.chunks(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    std::fs::write(out, ppm)?;
    println!("wrote {out}");
    Ok(())
}
