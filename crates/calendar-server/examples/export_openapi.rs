//! Export the same document served by HTTP, without opening a database or listener.
fn main() -> Result<(), serde_json::Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(&calendar_server::openapi())?
    );
    Ok(())
}
