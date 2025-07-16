use std::io::Result;

fn main() -> Result<()> {
    println!("writing hi.txt...");
    std::fs::write("hi.txt", "hi!")?;
    println!("reading hi.txt...");
    let content = std::fs::read("hi.txt")?;
    println!("hi.txt: {:?}", content);

    Ok(())
}
